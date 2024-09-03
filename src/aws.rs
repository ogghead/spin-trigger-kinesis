use aws_sdk_kinesis::{types::Record, Client};

pub struct ShardPoller<'a> {
    client: Client,
    stream_arn: &'a str,
    shard_id: &'a str,
    shard_iterator: Option<String>,
    shard_record_limit: u16,
}

impl<'a> ShardPoller<'a> {
    /// Try to create a new poller for the given stream and shard
    pub async fn try_new(
        client: Client,
        stream_arn: &'a str,
        shard_id: &'a str,
        shard_record_limit: u16,
    ) -> anyhow::Result<Self> {
        let shard_iterator = client
            .get_shard_iterator()
            .stream_arn(stream_arn)
            .shard_id(shard_id)
            .shard_iterator_type(aws_sdk_kinesis::types::ShardIteratorType::Latest)
            .send()
            .await
            .map(|res| res.shard_iterator)
            .map_err(anyhow::Error::from)?;

        Ok(Self {
            client,
            stream_arn,
            shard_id,
            shard_iterator,
            shard_record_limit,
        })
    }

    /// Get records from the shard using this poller. This will return an empty vector if there is no shard iterator
    pub async fn get_records(&mut self) -> Vec<Record> {
        if let Some(shard_iterator) = &self.shard_iterator {
            match self
                .client
                .get_records()
                .shard_iterator(shard_iterator.clone())
                .stream_arn(self.stream_arn)
                .limit(self.shard_record_limit.into())
                .send()
                .await
            {
                Ok(output) => {
                    self.shard_iterator = output.next_shard_iterator;
                    output.records
                }
                Err(e) => {
                    tracing::trace!("Got error while fetching records from shard {}", e);
                    vec![]
                }
            }
        } else {
            tracing::trace!(
                "Null shard iterator for poller {}. TODO: We should stop polling this iterator",
                self.shard_id
            );
            vec![]
        }
    }
}
