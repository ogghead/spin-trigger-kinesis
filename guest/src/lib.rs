wit_bindgen::generate!({
    world: "spin-kinesis",
    path: "..",
    exports: {
        world: Kinesis
    }
});

struct Kinesis;

impl Guest for Kinesis {
    fn handle_batch_records(records: Vec<KinesisRecord>) -> Result<(), Error> {
        for record in records {
            println!("I GOT A RECORD!  ID: {:?}", record.sequence_number);
            let data = String::from_utf8(record.data.inner).unwrap();
            println!("  ... DATA: {:?}", data);
        }

        Ok(())
    }
}
