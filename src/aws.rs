use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::Result;
use aws_sdk_kinesis::{
    operation::{get_records::GetRecordsOutput, list_shards::ListShardsOutput},
    types::{Shard, ShardIteratorType},
    Client,
};
use spin_factors::RuntimeFactors;
use spin_trigger::TriggerApp;
use tracing::{instrument, Instrument};

use crate::{KinesisRecordProcessor, KinesisTrigger};

pub struct ShardProcessor<F: RuntimeFactors> {
    app: Arc<TriggerApp<KinesisTrigger, F>>,
    component_id: Arc<String>,
    tx_finished_shard: tokio::sync::mpsc::Sender<String>,
    kinesis_client: Client,
    stream_arn: Arc<String>,
    shard_id: String,
    batch_size: u16,
    shard_idle_wait_millis: Duration,
    shard_iterator_type: ShardIteratorType,
}

impl<F: RuntimeFactors> ShardProcessor<F> {
    /// Try to create a new poller for the given stream and shard
    #[instrument(name = "spin_trigger_kinesis.new_shard_processor", skip_all, fields(otel.name = format!("new_shard_processor {}", component_id)))]
    pub fn new(
        app: &Arc<TriggerApp<KinesisTrigger, F>>,
        component_id: &Arc<String>,
        tx_finished_shard: &tokio::sync::mpsc::Sender<String>,
        kinesis_client: &Client,
        stream_arn: &Arc<String>,
        shard_id: String,
        batch_size: u16,
        shard_idle_wait_millis: Duration,
        shard_iterator_type: ShardIteratorType,
    ) -> Self {
        Self {
            app: app.clone(),
            component_id: component_id.clone(),
            tx_finished_shard: tx_finished_shard.clone(),
            kinesis_client: kinesis_client.clone(),
            stream_arn: stream_arn.clone(),
            shard_id,
            batch_size,
            shard_idle_wait_millis,
            shard_iterator_type,
        }
    }

    /// Get records from the shard using this poller. This will return an empty vector if there is no shard iterator
    pub async fn poll(self) {
        match self.get_shard_iterator().await {
            Some(mut shard_iterator) => {
                while let Some(new_shard_iterator) = self.process_batch(shard_iterator).await {
                    shard_iterator = new_shard_iterator;
                }
            }
            None => {
                tracing::error!(
                    "[Kinesis] Null shard iterator for poller {} on component {}. Exiting poll loop",
                    self.shard_id,
                    self.component_id,
                );
            }
        }

        self.close().await;
    }

    #[instrument(name = "spin_trigger_kinesis.close_shard_processor", skip_all, fields(otel.name = format!("close_shard_processor {}", self.component_id)))]
    async fn close(self) {
        if let Err(err) = self.tx_finished_shard.send(self.shard_id).await {
            tracing::error!("[Kinesis] Error sending shard id to finished channel: {err}");
        }
    }

    #[instrument(name = "spin_trigger_kinesis.get_shard_iterator", skip_all, fields(otel.name = format!("get_shard_iterator {}", self.component_id)))]
    async fn get_shard_iterator(&self) -> Option<String> {
        self.kinesis_client
            .get_shard_iterator()
            .stream_arn(self.stream_arn.as_ref())
            .shard_id(&self.shard_id)
            .shard_iterator_type(self.shard_iterator_type.clone())
            .send()
            .await
            .map(|res| res.shard_iterator)
            .inspect_err(|e| tracing::trace!("[Kinesis] Error creating shard iterator: {}", e))
            .unwrap_or_default()
    }

    #[instrument(name = "spin_trigger_kinesis.get_records", skip_all, fields(otel.name = format!("get_records {}", self.component_id)))]
    async fn get_records(&self, shard_iterator: impl Into<String>) -> Result<GetRecordsOutput> {
        Ok(self
            .kinesis_client
            .get_records()
            .shard_iterator(shard_iterator)
            .stream_arn(self.stream_arn.as_ref())
            .limit(self.batch_size.into())
            .send()
            .await?)
    }

    /// Poll for a batch of records to process
    #[instrument(name = "spin_trigger_kinesis.process_batch", skip_all, fields(otel.name = format!("process_batch {}", self.component_id)))]
    async fn process_batch(&self, shard_iterator: impl Into<String>) -> Option<String> {
        match self.get_records(shard_iterator).in_current_span().await {
            Ok(GetRecordsOutput {
                records,
                next_shard_iterator,
                millis_behind_latest,
                ..
            }) => {
                if records.is_empty() && millis_behind_latest.unwrap_or_default() == 0 {
                    tokio::time::sleep(self.shard_idle_wait_millis)
                        .instrument(tracing::info_span!(
                            "spin_trigger_kinesis.process_batch_idle",
                            "otel.name" = format!("process_batch_idle {}", self.component_id)
                        ))
                        .await;
                } else if !records.is_empty() {
                    let processor = KinesisRecordProcessor::new(&self.app, &self.component_id);
                    // Wait until processing is completed for these records
                    processor.process_records(records).in_current_span().await
                }
                next_shard_iterator
            }
            Err(err) => {
                tracing::trace!(
                    "[Kinesis] Got error while fetching records from shard {}: {err}",
                    self.shard_id
                );
                None
            }
        }
    }
}

pub struct ShardDetector {
    stream_arn: Arc<String>,
    detector_poll_millis: Duration,
    client: Client,
    rx_finished_shard: tokio::sync::mpsc::Receiver<String>,
    tx_new_shard: tokio::sync::mpsc::Sender<String>,
    running_shards: HashSet<String>,
    component_id: Arc<String>,
}

impl ShardDetector {
    /// Create a new shard detector for the given stream
    #[instrument(name = "spin_trigger_kinesis.new_shard_detector", skip_all, fields(otel.name = format!("new_shard_detector {}", component_id)))]
    pub fn new(
        stream_arn: &Arc<String>,
        detector_poll_millis: Duration,
        client: &Client,
        rx_finished_shard: tokio::sync::mpsc::Receiver<String>,
        tx_new_shard: tokio::sync::mpsc::Sender<String>,
        component_id: &Arc<String>,
    ) -> Self {
        Self {
            stream_arn: stream_arn.clone(),
            detector_poll_millis,
            client: client.clone(),
            rx_finished_shard,
            tx_new_shard,
            component_id: component_id.clone(),
            running_shards: HashSet::new(),
        }
    }

    /// Poll for new shards -- if we find new shards, we need to create a new shard poller for each
    pub async fn poll_new_shards(mut self) {
        loop {
            if !self.rx_finished_shard.is_empty() {
                match self.rx_finished_shard.recv().await {
                    Some(shard) => {
                        self.running_shards.remove(&shard);
                    }
                    None => {
                        tracing::error!("[Kinesis] Finished shard channel closed.");
                    }
                }
            } else {
                match self
                    .client
                    .list_shards()
                    .stream_arn(self.stream_arn.as_ref())
                    .send()
                    .await
                {
                    Ok(ListShardsOutput {
                        shards: Some(shards),
                        ..
                    }) => {
                        for Shard { shard_id, .. } in shards {
                            if self.running_shards.contains(&shard_id) {
                                continue;
                            }

                            if let Err(e) = self.tx_new_shard.send(shard_id.clone()).await {
                                tracing::error!(
                                    "[Kinesis] Error sending new shard to main loop: {:?}",
                                    e
                                );
                            } else {
                                self.running_shards.insert(shard_id);
                            }
                        }
                    }
                    Ok(ListShardsOutput { shards: None, .. }) => {
                        tracing::error!(
                            "[Kinesis] No shards found in stream for {}",
                            self.component_id
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            "[Kinesis] Error listing shards in detector for {}: {:?}",
                            self.component_id,
                            e
                        );
                    }
                }
                tokio::time::sleep(self.detector_poll_millis)
                    .instrument(tracing::info_span!(
                        "spin_trigger_kinesis.detector_poll_idle",
                        "otel.name" = format!("detector_poll_idle {}", self.component_id)
                    ))
                    .await;
            }
        }
    }
}
