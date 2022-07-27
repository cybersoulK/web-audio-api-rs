use std::fs::File;
use web_audio_api::context::{AudioContext, BaseAudioContext};
use web_audio_api::node::{AudioNode, AudioScheduledSourceNode, DynamicsCompressorNode};

fn main() {
    env_logger::init();

    println!("> gradually increase the amount of compression applied on the sample");

    let context = AudioContext::default();

    let file = File::open("samples/sample.wav").unwrap();
    let buffer = context.decode_audio_data_sync(file).unwrap();

    let src = context.create_buffer_source();
    src.connect(&context.destination());
    src.set_buffer(buffer.clone());
    src.start();

    // enjoy listening
    std::thread::sleep(std::time::Duration::from_secs(4));

    for i in 0..7 {
        let compressor = DynamicsCompressorNode::new(&context, Default::default());
        compressor.connect(&context.destination());
        compressor.threshold().set_value(-10. * i as f32);
        compressor.knee().set_value(0.); // hard knee
        compressor.attack().set_value(0.05); // hard knee
        compressor.release().set_value(0.1); // hard knee

        let src = context.create_buffer_source();
        src.connect(&compressor);
        src.set_buffer(buffer.clone());
        src.start();

        // enjoy listening
        std::thread::sleep(std::time::Duration::from_secs(4));
    }
}
