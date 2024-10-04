use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::Result;
use aws_sdk_kinesis::{
    operation::{get_records::GetRecordsOutput, list_shards::ListShardsOutput},
    types::{Shard, ShardIteratorType},
    Client,
};
use spin_trigger::TriggerAppEngine;
use tracing::{instrument, Instrument};

use crate::{KinesisRecordProcessor, KinesisTrigger};

pub struct ShardProcessor {
    engine: Arc<TriggerAppEngine<KinesisTrigger>>,
    component_id: Arc<String>,
    tx_finished_shard: tokio::sync::mpsc::Sender<String>,
    kinesis_client: Client,
    stream_arn: Arc<String>,
    shard_id: String,
    batch_size: u16,
    shard_idle_wait_millis: Duration,
    shard_iterator_type: ShardIteratorType,
}

impl ShardProcessor {
    /// Try to create a new poller for the given stream and shard
    #[instrument(name = "spin_trigger_kinesis.new_shard_processor", skip_all, fields(otel.name = format!("new_shard_processor {}", component_id)))]
    pub fn new(
        engine: &Arc<TriggerAppEngine<KinesisTrigger>>,
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
            engine: engine.clone(),
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
                    let processor = KinesisRecordProcessor::new(&self.engine, &self.component_id);
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

    /// Get records from the shard using this poller. This will return an empty vector if there is no shard iterator
    pub async fn poll(self) {
        let Some(mut shard_iterator) = self.get_shard_iterator().await else {
            tracing::trace!(
                "[Kinesis] Null shard iterator for poller {}. Exiting poll loop",
                self.shard_id
            );
            return;
        };

        while let Some(new_shard_iterator) = self.process_batch(shard_iterator).await {
            shard_iterator = new_shard_iterator;
        }

        if let Err(err) = self.tx_finished_shard.send(self.shard_id).await {
            tracing::error!("[Kinesis] Error sending shard id to finished channel: {err}");
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
            running_shards: HashSet::new(),
        }
    }

    /// Poll for new shards -- if we find new shards, we need to create a new shard poller for each
    pub async fn poll_new_shards(mut self) {
        loop {
            tokio::select! {
                Some(shard) = self.rx_finished_shard.recv() => {
                    self.running_shards.remove(&shard);
                },
                Ok(ListShardsOutput { shards: Some(new_shards), .. }) = self
                    .client
                    .list_shards()
                    .stream_arn(self.stream_arn.as_ref())
                    .send() => {
                        for Shard {shard_id, ..} in new_shards {
                            if self.running_shards.contains(&shard_id) {
                                continue;
                            }

                            if let Err(e) = self.tx_new_shard.send(shard_id.clone()).await {
                                tracing::error!(
                                    "[Kinesis] Error sending new shard to main loop: {:?}",
                                    e
                                );
                                return;
                            } else {
                                self.running_shards.insert(shard_id);
                            }
                        }
                        tokio::time::sleep(self.detector_poll_millis).await;
                },
                else => {
                    tracing::error!(
                        "[Kinesis] Unable to fetch shards from iterator. Shard detector exiting."
                    );
                    return;
                }
            }
        }
    }
}
