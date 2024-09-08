use std::{collections::HashSet, sync::Arc, time::Duration};

use aws_sdk_kinesis::{operation::list_shards::ListShardsOutput, Client};
use spin_trigger::TriggerAppEngine;

use crate::{KinesisRecordProcessor, KinesisTrigger};

pub struct ShardPoller {
    engine: Arc<TriggerAppEngine<KinesisTrigger>>,
    component_id: Arc<String>,
    tx_finished_shard: tokio::sync::mpsc::Sender<String>,
    client: Client,
    stream_arn: Arc<String>,
    shard_id: String,
    batch_size: u16,
    shard_idle_wait_seconds: Duration,
}

impl ShardPoller {
    /// Try to create a new poller for the given stream and shard
    pub fn new(
        engine: &Arc<TriggerAppEngine<KinesisTrigger>>,
        component_id: &Arc<String>,
        tx_finished_shard: &tokio::sync::mpsc::Sender<String>,
        client: &Client,
        stream_arn: &Arc<String>,
        shard_id: String,
        batch_size: u16,
        shard_idle_wait_seconds: Duration,
    ) -> Self {
        let client = client.clone();

        Self {
            engine: engine.clone(),
            component_id: component_id.clone(),
            tx_finished_shard: tx_finished_shard.clone(),
            client: client.clone(),
            stream_arn: stream_arn.clone(),
            shard_id,
            batch_size,
            shard_idle_wait_seconds,
        }
    }

    async fn get_shard_iterator(&self) -> Option<String> {
        self.client
            .get_shard_iterator()
            .stream_arn(self.stream_arn.as_ref())
            .shard_id(&self.shard_id)
            .shard_iterator_type(aws_sdk_kinesis::types::ShardIteratorType::Latest)
            .send()
            .await
            .map(|res| res.shard_iterator)
            .inspect_err(|e| tracing::trace!("Error creating shard iterator: {}", e))
            .unwrap_or_default()
    }

    /// Get records from the shard using this poller. This will return an empty vector if there is no shard iterator
    pub async fn poll_records(self) {
        let mut shard_iterator = self.get_shard_iterator().await;
        loop {
            if let Some(iterator) = shard_iterator {
                match self
                    .client
                    .get_records()
                    .shard_iterator(iterator)
                    .stream_arn(self.stream_arn.as_ref())
                    .limit(self.batch_size.into())
                    .send()
                    .await
                {
                    Ok(output) => {
                        shard_iterator = output.next_shard_iterator;
                        if output.records.is_empty() {
                            tokio::time::sleep(self.shard_idle_wait_seconds).await;
                        } else if !output.records.is_empty() {
                            let processor =
                                KinesisRecordProcessor::new(&self.engine, &self.component_id);
                            // Wait until processing is completed for these records
                            processor.process_records(output.records).await
                        }
                    }
                    Err(e) => {
                        tracing::trace!("Got error while fetching records from shard {}", e);
                        let _ = self
                            .tx_finished_shard
                            .send(self.shard_id)
                            .await
                            .inspect_err(|e| {
                                tracing::error!("Error sending shard id to finished channel: {}", e)
                            });
                        return;
                    }
                }
            } else {
                tracing::trace!(
                    "Null shard iterator for poller {}. Exiting poll loop",
                    self.shard_id
                );
                let _ = self
                    .tx_finished_shard
                    .send(self.shard_id)
                    .await
                    .inspect_err(|e| {
                        tracing::error!("Error sending shard id to finished channel: {}", e)
                    });
                return;
            }
        }
    }
}

pub struct ShardDetector {
    stream_arn: Arc<String>,
    detector_poll_seconds: Duration,
    client: Client,
    rx_finished_shard: tokio::sync::mpsc::Receiver<String>,
    tx_new_shard: tokio::sync::mpsc::Sender<String>,
    running_shards: HashSet<String>,
}

impl ShardDetector {
    /// Create a new shard detector for the given stream
    pub fn new(
        stream_arn: &Arc<String>,
        detector_poll_seconds: Duration,
        client: &Client,
        rx_finished_shard: tokio::sync::mpsc::Receiver<String>,
        tx_new_shard: tokio::sync::mpsc::Sender<String>,
    ) -> Self {
        Self {
            stream_arn: stream_arn.clone(),
            detector_poll_seconds,
            client: client.clone(),
            rx_finished_shard,
            tx_new_shard,
            running_shards: HashSet::new(),
        }
    }

    pub async fn poll_new_shards(mut self) {
        loop {
            if !self.rx_finished_shard.is_empty() {
                match self.rx_finished_shard.recv().await {
                    Some(shard) => {
                        self.running_shards.remove(&shard);
                    }
                    None => {
                        tracing::trace!("Shard detector: finished shard channel closed");
                        return;
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
                        shards: Some(new_shards),
                        ..
                    }) => {
                        let shards = new_shards
                            .into_iter()
                            .filter(|shard| !self.running_shards.contains(&shard.shard_id))
                            .map(|shard| shard.shard_id)
                            .collect::<Vec<_>>();
                        for shard in shards {
                            if let Err(e) = self.tx_new_shard.send(shard.clone()).await {
                                tracing::error!("Error sending new shard to poller: {:?}", e);
                            } else {
                                self.running_shards.insert(shard);
                            }
                        }
                    }
                    Ok(ListShardsOutput { shards: None, .. }) => {
                        tracing::trace!("Stream {}: no shards returned", self.stream_arn);
                        return;
                    }
                    Err(e) => {
                        tracing::error!(
                            "Stream {}: error listing shards: {:?}",
                            self.stream_arn,
                            e
                        );
                        return;
                    }
                }
                tokio::time::sleep(self.detector_poll_seconds).await;
            }
        }
    }
}
