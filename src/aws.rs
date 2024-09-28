use std::{collections::HashSet, sync::Arc, time::Duration};

use aws_sdk_kinesis::{
    operation::{get_records::GetRecordsOutput, list_shards::ListShardsOutput},
    types::ShardIteratorType,
    Client,
};
use spin_trigger::TriggerAppEngine;

use crate::{KinesisRecordProcessor, KinesisTrigger};

pub struct ShardPoller {
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

impl ShardPoller {
    /// Try to create a new poller for the given stream and shard
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

    /// Get records from the shard using this poller. This will return an empty vector if there is no shard iterator
    pub async fn poll_records(self) {
        let mut shard_iterator = self.get_shard_iterator().await;
        loop {
            let Some(iterator) = shard_iterator else {
                tracing::trace!(
                    "[Kinesis] Null shard iterator for poller {}. Exiting poll loop",
                    self.shard_id
                );
                break;
            };

            match self
                .kinesis_client
                .get_records()
                .shard_iterator(iterator)
                .stream_arn(self.stream_arn.as_ref())
                .limit(self.batch_size.into())
                .send()
                .await
            {
                Ok(GetRecordsOutput {
                    records,
                    next_shard_iterator,
                    millis_behind_latest,
                    // child_shards, TODO
                    ..
                }) => {
                    shard_iterator = next_shard_iterator;

                    // if let Some(child_shards) = child_shards {
                    //     for child_shard in child_shards {
                    //         let shard_poller = ShardPoller::new(
                    //             &self.engine,
                    //             &self.component_id,
                    //             &self.tx_finished_shard,
                    //             &self.kinesis_client,
                    //             &self.stream_arn,
                    //             child_shard.shard_id,
                    //             self.batch_size,
                    //             self.shard_idle_wait_millis,
                    //             self.shard_iterator_type,
                    //         );
                    //         tokio::spawn(shard_poller.poll_records());
                    //     }
                    // }

                    if records.is_empty() && millis_behind_latest.unwrap_or_default() == 0 {
                        tokio::time::sleep(self.shard_idle_wait_millis).await;
                    } else if !records.is_empty() {
                        let processor =
                            KinesisRecordProcessor::new(&self.engine, &self.component_id);
                        // Wait until processing is completed for these records
                        processor.process_records(records).await
                    }
                }
                Err(e) => {
                    tracing::trace!(
                        "[Kinesis] Got error while fetching records from shard {}",
                        e
                    );
                    break;
                }
            }
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
    pub fn new(
        stream_arn: &Arc<String>,
        detector_poll_millis: Duration,
        client: &Client,
        rx_finished_shard: tokio::sync::mpsc::Receiver<String>,
        tx_new_shard: tokio::sync::mpsc::Sender<String>,
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
                        let shards = new_shards
                            .into_iter()
                            .filter(|shard| !self.running_shards.contains(&shard.shard_id))
                            .map(|shard| shard.shard_id)
                            .collect::<Vec<_>>();
                        for shard in shards {
                            if let Err(e) = self.tx_new_shard.send(shard.clone()).await {
                                tracing::error!(
                                    "[Kinesis] Error sending new shard to poller: {:?}",
                                    e
                                );
                            } else {
                                self.running_shards.insert(shard);
                            }
                        }
                        tokio::time::sleep(self.detector_poll_millis).await;
                },
                else => break
            }
        }
    }
}
