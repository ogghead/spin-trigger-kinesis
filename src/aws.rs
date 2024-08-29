use std::marker::PhantomData;

use aws_sdk_kinesis::{types::Record, Client};

pub struct New;
pub struct Ready;

pub struct ShardPoller<'a, PollerState = New> {
    client: Client,
    stream_arn: &'a str,
    shard_id: &'a str,
    shard_iterator: Option<String>,
    shard_record_limit: u16,
    _state: PhantomData<PollerState>,
}

impl<'a> ShardPoller<'a, New> {
    pub fn new(
        client: Client,
        stream_arn: &'a str,
        shard_id: &'a str,
        shard_record_limit: u16,
    ) -> Self {
        Self {
            client,
            stream_arn,
            shard_id,
            shard_iterator: None,
            shard_record_limit,
            _state: PhantomData,
        }
    }

    pub async fn make_ready(mut self) -> anyhow::Result<ShardPoller<'a, Ready>> {
        let shard_iterator = self
            .client
            .get_shard_iterator()
            .stream_arn(self.stream_arn)
            .shard_id(self.shard_id)
            .shard_iterator_type(aws_sdk_kinesis::types::ShardIteratorType::Latest)
            .send()
            .await
            .map(|res| res.shard_iterator)
            .map_err(anyhow::Error::from)?;
        self.shard_iterator = shard_iterator;

        Ok(ShardPoller {
            client: self.client,
            stream_arn: self.stream_arn,
            shard_id: self.shard_id,
            shard_iterator: self.shard_iterator,
            shard_record_limit: self.shard_record_limit,
            _state: PhantomData,
        })
    }
}

impl ShardPoller<'_, Ready> {
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
            tracing::trace!("No records in shard {}", self.shard_id);
            vec![]
        }
    }
}
