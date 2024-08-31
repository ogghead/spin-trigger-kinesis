# Experimental Kinesis trigger for Spin

## Install the latest release

The latest stable release of the kinesis trigger plugin can be installed like so:

```sh
spin plugins update
spin plugins install trigger-kinesis
```

## Install the canary version

The canary release of the kinesis trigger plugin represents the most recent commits on `main` and may not be stable, with some features still in progress.

```sh
spin plugins install --url https://github.com/ogghead/spin-trigger-kinesis/releases/download/canary/trigger-kinesis.json
```

## Build from source

You will need Rust and the `pluginify` plugin (`spin plugins install --url https://github.com/itowlson/spin-pluginify/releases/download/canary/pluginify.json`).

```
cargo build --release
spin pluginify --install
```

## Test

The trigger uses the AWS configuration environment variables - these must be set before running.
Be sure to set `AWS_DEFAULT_REGION` in your environment to the region of your stream.

You will also need to change the `stream_arn` in `spin.toml` to a stream you have access to.

```
cd guest
spin build --up
```

## Limitations

This trigger is currently built using Spin 2.0.1. You will need that version of Spin or above.

Custom triggers, such as this one, can be run in the Spin command line, but cannot be deployed to Fermyon Cloud.  For other hosts, check the documentation.

## Configuration

The kinesis trigger uses the AWS credentials from the standard AWS configuration environment variables.  These variables must be set before you run `spin up`.  The credentials must grant access to all streams that the application wants to monitor.  The credentials must allow for reading records.

The trigger assumes that the monitored streams exist: it does not create them.

### `spin.toml`

The trigger type is `kinesis`, and there are no application-level configuration options.

The following options are available to set in the `[[trigger.kinesis]]` section:

| Name                  | Type             | Required? | Description |
|-----------------------|------------------|-----------|-------------|
| `stream_arn`          | string           | required | The stream to which this trigger listens and responds. |
| `shard_record_limit`        | number           | optional | The maximum number of records to fetch per Kinesis shard on each poll. The default is 10. Note that the total number of records per poll is equal to shard_record_limit * number of shards. This refers specifically to how records are fetched from AWS - the component is still invoked separately for each record. |
| `idle_wait_seconds`   | number           | optional | How long (in seconds) to wait between checks when the stream is idle (i.e. when no messages were received on the last check). The default is 2. If the stream is _not_ idle, there is no wait between checks. The idle wait is also applied if an error occurs. |
| `component`           | string or table  | required | The component to run when a stream record is received. (This is the standard Spin trigger component field.) |

For example:

```toml
spin_manifest_version = 2

[application]
name = "test"
version = "0.1.0"

# One [[trigger.kinesis]] section for each stream to monitor
[[trigger.kinesis]]
stream_arn = "arn:aws:kinesis:us-east-1:1234567890:stream/TestStream"
shard_record_limit = 10
idle_wait_seconds = 10

[component.test]
source = "..."
```

### `spin up` command line options

There are no custom command line options for this trigger.

## Writing kinesis components

There is no SDK for kinesis guest components.  Use the `kinesis.wit` file to generate a trigger binding for your language.  Your Wasm component must _export_ the `handle-stream-message` function.  See `guest/src/lib.rs`  for how to do this in Rust.

**Note:** In the current WIT, a record has a single `data` field. This contains the content of the record encoded as binary. Feedback is welcome on this design decision.

Your handler can an error, but should otherwise not return anything.

**Note:** The trigger currently processes records using ShardIteratorType::Latest. This means that only records published after the app is running will be read.

**Note:** The trigger continues to poll shards while a handler is running. This means that records are not necessarily processed sequentially.
