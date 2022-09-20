use std::f32::consts::PI;
use std::sync::Arc;

use crate::context::{AudioContextRegistration, AudioParamId, BaseAudioContext};
use crate::param::{AudioParam, AudioParamDescriptor};
use crate::render::{AudioParamValues, AudioProcessor, AudioRenderQuantum, RenderScope};
use crate::{AtomicF64, RENDER_QUANTUM_SIZE};

use super::{
    AudioNode, ChannelConfig, ChannelConfigOptions, ChannelCountMode, ChannelInterpretation,
};

use float_eq::float_eq;
use hrtf::{HrirSphere, HrtfContext, HrtfProcessor, Vec3};

/// Spatialization algorithm used to position the audio in 3D space
#[derive(Copy, Clone, Debug)]
pub enum PanningModelType {
    EqualPower,
    HRTF,
}

/// Algorithm to reduce the volume of an audio source as it moves away from the listener
#[derive(Copy, Clone, Debug)]
pub enum DistanceModelType {
    Linear,
    Inverse,
    Exponential,
}

/// Options for constructing a [`PannerNode`]
// dictionary PannerOptions : AudioNodeOptions {
//   PanningModelType panningModel = "equalpower";
//   DistanceModelType distanceModel = "inverse";
//   float positionX = 0;
//   float positionY = 0;
//   float positionZ = 0;
//   float orientationX = 1;
//   float orientationY = 0;
//   float orientationZ = 0;
//   double refDistance = 1;
//   double maxDistance = 10000;
//   double rolloffFactor = 1;
//   double coneInnerAngle = 360;
//   double coneOuterAngle = 360;
//   double coneOuterGain = 0;
// };
#[derive(Clone, Debug)]
pub struct PannerOptions {
    pub panning_model: PanningModelType,
    #[allow(dead_code)]
    pub distance_model: DistanceModelType,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub orientation_x: f32,
    pub orientation_y: f32,
    pub orientation_z: f32,
    #[allow(dead_code)]
    pub ref_distance: f64,
    #[allow(dead_code)]
    pub max_distance: f64,
    #[allow(dead_code)]
    pub rolloff_factor: f64,
    pub cone_inner_angle: f64,
    pub cone_outer_angle: f64,
    pub cone_outer_gain: f64,
}

impl Default for PannerOptions {
    fn default() -> Self {
        PannerOptions {
            panning_model: PanningModelType::EqualPower,
            distance_model: DistanceModelType::Inverse,
            position_x: 0.,
            position_y: 0.,
            position_z: 0.,
            orientation_x: 1.,
            orientation_y: 0.,
            orientation_z: 0.,
            ref_distance: 1.,
            max_distance: 10000.,
            rolloff_factor: 1.,
            cone_inner_angle: 360.,
            cone_outer_angle: 360.,
            cone_outer_gain: 0.,
        }
    }
}

struct HrtfState {
    processor: HrtfProcessor,
    output_interleaved: Vec<(f32, f32)>,
    prev_sample_vector: Vec3,
    prev_left_samples: Vec<f32>,
    prev_right_samples: Vec<f32>,
    prev_distance_gain: f32,
}

impl HrtfState {
    fn new(hrir_sphere: HrirSphere) -> Self {
        let interpolation_steps = 1;
        let samples_per_step = RENDER_QUANTUM_SIZE / interpolation_steps;

        let processor = HrtfProcessor::new(hrir_sphere, interpolation_steps, samples_per_step);

        Self {
            processor,
            output_interleaved: vec![(0., 0.); RENDER_QUANTUM_SIZE],
            prev_sample_vector: Vec3::new(0., 0., 1.),
            prev_left_samples: vec![],  // will resize accordingly
            prev_right_samples: vec![], // will resize accordingly
            prev_distance_gain: 0.,
        }
    }

    fn process(
        &mut self,
        source: &[f32],
        new_distance_gain: f32,
        projected_source: [f32; 3],
    ) -> &[(f32, f32)] {
        let new_sample_vector = Vec3 {
            x: projected_source[0],
            z: projected_source[1],
            y: projected_source[2],
        };

        let context = HrtfContext {
            source,
            output: &mut self.output_interleaved,
            new_sample_vector,
            prev_sample_vector: self.prev_sample_vector,
            prev_left_samples: &mut self.prev_left_samples,
            prev_right_samples: &mut self.prev_right_samples,
            new_distance_gain,
            prev_distance_gain: self.prev_distance_gain,
        };

        self.processor.process_samples(context);

        self.prev_sample_vector = new_sample_vector;
        self.prev_distance_gain = new_distance_gain;

        &self.output_interleaved
    }
}

/// Node that positions / spatializes an incoming audio stream in three-dimensional space.
///
/// - MDN documentation: <https://developer.mozilla.org/en-US/docs/Web/API/PannerNode>
/// - specification: <https://www.w3.org/TR/webaudio/#pannernode> and
/// <https://www.w3.org/TR/webaudio/#Spatialization>
/// - see also:
/// [`BaseAudioContext::create_panner`](crate::context::BaseAudioContext::create_panner)
///
/// # Usage
/// ```no_run
/// use web_audio_api::context::{BaseAudioContext, AudioContext};
/// use web_audio_api::node::AudioNode;
/// use web_audio_api::node::AudioScheduledSourceNode;
///
/// // Setup a new audio context
/// let context = AudioContext::default();
///
/// // Create a friendly tone
/// let tone = context.create_oscillator();
/// tone.frequency().set_value_at_time(300.0f32, 0.);
/// tone.start();
///
/// // Connect tone > panner node > destination node
/// let panner = context.create_panner();
/// tone.connect(&panner);
/// panner.connect(&context.destination());
///
/// // The panner node is 1 unit in front of listener
/// panner.position_z().set_value_at_time(1., 0.);
///
/// // And sweeps 10 units left to right, every second
/// let moving = context.create_oscillator();
/// moving.start();
/// moving.frequency().set_value_at_time(1., 0.);
/// let gain = context.create_gain();
/// gain.gain().set_value_at_time(10., 0.);
/// moving.connect(&gain);
/// gain.connect(panner.position_x());
///
/// // enjoy listening
/// std::thread::sleep(std::time::Duration::from_secs(4));
/// ```
///
/// # Examples
///
/// - `cargo run --release --example spatial`
/// - `cargo run --release --example panner_cone`
pub struct PannerNode {
    registration: AudioContextRegistration,
    channel_config: ChannelConfig,
    position_x: AudioParam,
    position_y: AudioParam,
    position_z: AudioParam,
    orientation_x: AudioParam,
    orientation_y: AudioParam,
    orientation_z: AudioParam,
    cone_inner_angle: Arc<AtomicF64>,
    cone_outer_angle: Arc<AtomicF64>,
    cone_outer_gain: Arc<AtomicF64>,
}

impl AudioNode for PannerNode {
    fn registration(&self) -> &AudioContextRegistration {
        &self.registration
    }

    fn channel_config(&self) -> &ChannelConfig {
        &self.channel_config
    }

    fn number_of_inputs(&self) -> usize {
        1 + 9 // todo, user should not be able to see these ports
    }

    fn number_of_outputs(&self) -> usize {
        1
    }

    fn set_channel_count(&self, v: usize) {
        if v > 2 {
            panic!("NotSupportedError: PannerNode channel count cannot be greater than two");
        }
        self.channel_config.set_count(v);
    }

    fn set_channel_count_mode(&self, v: ChannelCountMode) {
        if v == ChannelCountMode::Max {
            panic!("NotSupportedError: PannerNode channel count mode cannot be set to max");
        }
        self.channel_config.set_count_mode(v);
    }
}

impl PannerNode {
    // can panic when loading HRIR-sphere
    #[allow(clippy::missing_panics_doc)]
    pub fn new<C: BaseAudioContext>(context: &C, options: PannerOptions) -> Self {
        let node = context.register(move |registration| {
            use crate::spatial::PARAM_OPTS;
            // position params
            let (position_x, render_px) = context.create_audio_param(PARAM_OPTS, &registration);
            let (position_y, render_py) = context.create_audio_param(PARAM_OPTS, &registration);
            let (position_z, render_pz) = context.create_audio_param(PARAM_OPTS, &registration);
            position_x.set_value_at_time(options.position_x, 0.);
            position_y.set_value_at_time(options.position_y, 0.);
            position_z.set_value_at_time(options.position_z, 0.);

            // orientation params
            let orientation_x_opts = AudioParamDescriptor {
                default_value: 1.0,
                ..PARAM_OPTS
            };
            let (orientation_x, render_ox) =
                context.create_audio_param(orientation_x_opts, &registration);
            let (orientation_y, render_oy) = context.create_audio_param(PARAM_OPTS, &registration);
            let (orientation_z, render_oz) = context.create_audio_param(PARAM_OPTS, &registration);
            orientation_x.set_value_at_time(options.orientation_x, 0.);
            orientation_y.set_value_at_time(options.orientation_y, 0.);
            orientation_z.set_value_at_time(options.orientation_z, 0.);

            // cone attributes
            let cone_inner_angle = Arc::new(AtomicF64::new(options.cone_inner_angle));
            let cone_outer_angle = Arc::new(AtomicF64::new(options.cone_outer_angle));
            let cone_outer_gain = Arc::new(AtomicF64::new(options.cone_outer_gain));

            let hrtf_state = if let PanningModelType::HRTF = options.panning_model {
                // TODO - embed sphere in library or let user specify location
                let resource = include_bytes!("../../resources/IRC_1003_C.bin");
                let sample_rate = context.sample_rate() as u32;
                let hrir_sphere = HrirSphere::new(&resource[..], sample_rate).unwrap();

                Some(HrtfState::new(hrir_sphere))
            } else {
                None
            };

            let render = PannerRenderer {
                position_x: render_px,
                position_y: render_py,
                position_z: render_pz,
                orientation_x: render_ox,
                orientation_y: render_oy,
                orientation_z: render_oz,
                cone_inner_angle: cone_inner_angle.clone(),
                cone_outer_angle: cone_outer_angle.clone(),
                cone_outer_gain: cone_outer_gain.clone(),
                hrtf_state,
            };

            let node = PannerNode {
                registration,
                channel_config: ChannelConfigOptions {
                    count: 2,
                    mode: ChannelCountMode::ClampedMax,
                    interpretation: ChannelInterpretation::Speakers,
                }
                .into(),
                position_x,
                position_y,
                position_z,
                orientation_x,
                orientation_y,
                orientation_z,
                cone_inner_angle,
                cone_outer_angle,
                cone_outer_gain,
            };

            // instruct to BaseContext to add the AudioListener if it has not already
            context.base().ensure_audio_listener_present();

            (node, Box::new(render))
        });

        // after the node is registered, connect the AudioListener
        context
            .base()
            .connect_listener_to_panner(node.registration().id());

        node
    }

    pub fn position_x(&self) -> &AudioParam {
        &self.position_x
    }

    pub fn position_y(&self) -> &AudioParam {
        &self.position_y
    }

    pub fn position_z(&self) -> &AudioParam {
        &self.position_z
    }

    pub fn orientation_x(&self) -> &AudioParam {
        &self.orientation_x
    }

    pub fn orientation_y(&self) -> &AudioParam {
        &self.orientation_y
    }

    pub fn orientation_z(&self) -> &AudioParam {
        &self.orientation_z
    }

    pub fn cone_inner_angle(&self) -> f64 {
        self.cone_inner_angle.load()
    }

    pub fn set_cone_inner_angle(&self, value: f64) {
        self.cone_inner_angle.store(value);
    }

    pub fn cone_outer_angle(&self) -> f64 {
        self.cone_outer_angle.load()
    }

    pub fn set_cone_outer_angle(&self, value: f64) {
        self.cone_outer_angle.store(value);
    }

    pub fn cone_outer_gain(&self) -> f64 {
        self.cone_outer_gain.load()
    }

    pub fn set_cone_outer_gain(&self, value: f64) {
        self.cone_outer_gain.store(value);
    }
}

struct PannerRenderer {
    position_x: AudioParamId,
    position_y: AudioParamId,
    position_z: AudioParamId,
    orientation_x: AudioParamId,
    orientation_y: AudioParamId,
    orientation_z: AudioParamId,
    cone_inner_angle: Arc<AtomicF64>,
    cone_outer_angle: Arc<AtomicF64>,
    cone_outer_gain: Arc<AtomicF64>,
    hrtf_state: Option<HrtfState>,
}

impl AudioProcessor for PannerRenderer {
    fn process(
        &mut self,
        inputs: &[AudioRenderQuantum],
        outputs: &mut [AudioRenderQuantum],
        params: AudioParamValues,
        _scope: &RenderScope,
    ) -> bool {
        // single input/output node
        let input = &inputs[0];
        let output = &mut outputs[0];

        // pass through input
        *output = input.clone();

        // only handle mono for now (todo issue #44)
        output.mix(1, ChannelInterpretation::Speakers);

        // early exit for silence
        if input.is_silent() {
            return false;
        }

        // convert mono to identical stereo
        output.mix(2, ChannelInterpretation::Speakers);

        // K-rate processing for now (todo issue #44)

        // source parameters (Panner)
        let source_position_x = params.get(&self.position_x)[0];
        let source_position_y = params.get(&self.position_y)[0];
        let source_position_z = params.get(&self.position_z)[0];
        let source_orientation_x = params.get(&self.orientation_x)[0];
        let source_orientation_y = params.get(&self.orientation_y)[0];
        let source_orientation_z = params.get(&self.orientation_z)[0];

        // listener parameters (AudioListener)
        let l_position_x = inputs[1].channel_data(0)[0];
        let l_position_y = inputs[2].channel_data(0)[0];
        let l_position_z = inputs[3].channel_data(0)[0];
        let l_forward_x = inputs[4].channel_data(0)[0];
        let l_forward_y = inputs[5].channel_data(0)[0];
        let l_forward_z = inputs[6].channel_data(0)[0];
        let l_up_x = inputs[7].channel_data(0)[0];
        let l_up_y = inputs[8].channel_data(0)[0];
        let l_up_z = inputs[9].channel_data(0)[0];

        // define base vectors in 3D
        let source_position = [source_position_x, source_position_y, source_position_z];
        let source_orientation = [
            source_orientation_x,
            source_orientation_y,
            source_orientation_z,
        ];
        let listener_position = [l_position_x, l_position_y, l_position_z];
        let listener_forward = [l_forward_x, l_forward_y, l_forward_z];
        let listener_up = [l_up_x, l_up_y, l_up_z];

        // azimuth and elevation of listener <> panner.
        // elevation is not used in the equal power panningModel (todo issue #44)
        let (mut azimuth, elevation) = crate::spatial::azimuth_and_elevation(
            source_position,
            listener_position,
            listener_forward,
            listener_up,
        );

        // determine distance gain
        let distance = crate::spatial::distance(source_position, listener_position);
        let dist_gain = if distance > 0. {
            1. / distance // inverse distance model is assumed (todo issue #44)
        } else {
            1.
        };

        // determine cone effect gain
        let abs_inner_angle = self.cone_inner_angle.load().abs() as f32 / 2.;
        let abs_outer_angle = self.cone_outer_angle.load().abs() as f32 / 2.;
        let cone_gain = if abs_inner_angle >= 180. && abs_outer_angle >= 180. {
            1. // no cone specified
        } else {
            let cone_outer_gain = self.cone_outer_gain.load() as f32;

            let abs_angle =
                crate::spatial::angle(source_position, source_orientation, listener_position);

            if abs_angle < abs_inner_angle {
                1. // No attenuation
            } else if abs_angle >= abs_outer_angle {
                cone_outer_gain // Max attenuation
            } else {
                // Between inner and outer cones: inner -> outer, x goes from 0 -> 1
                let x = (abs_angle - abs_inner_angle) / (abs_outer_angle - abs_inner_angle);
                (1. - x) + cone_outer_gain * x
            }
        };

        if let Some(hrtf_state) = &mut self.hrtf_state {
            let new_distance_gain = cone_gain * dist_gain;

            // convert az/el to carthesian coordinates to determine unit direction
            let az_rad = azimuth * PI / 180.;
            let el_rad = elevation * PI / 180.;
            let x = az_rad.sin() * el_rad.cos();
            let z = az_rad.cos() * el_rad.cos();
            let y = el_rad.sin();
            let mut projected_source = [x, y, z];

            if float_eq!(&projected_source[..], &[0.; 3][..], abs_all <= 1E-6) {
                projected_source = [0., 0., 1.];
            }

            let output_interleaved = hrtf_state.process(
                output.channel_data(0).as_slice(),
                new_distance_gain,
                projected_source,
            );

            output_interleaved
                .iter()
                .zip(output.channel_data_mut(0).iter_mut())
                .for_each(|(p, l)| {
                    *l = p.0;
                });

            output_interleaved
                .iter()
                .zip(output.channel_data_mut(1).iter_mut())
                .for_each(|(p, r)| {
                    *r = p.1;
                });

            hrtf_state.output_interleaved.fill((0., 0.));
        } else {
            // Determine left/right ear gain. Clamp azimuth to range of [-180, 180].
            azimuth = azimuth.max(-180.);
            azimuth = azimuth.min(180.);

            // Then wrap to range [-90, 90].
            if azimuth < -90. {
                azimuth = -180. - azimuth;
            } else if azimuth > 90. {
                azimuth = 180. - azimuth;
            }

            // x is the horizontal plane orientation of the sound
            let x = (azimuth + 90.) / 180.;
            let gain_l = (x * PI / 2.).cos();
            let gain_r = (x * PI / 2.).sin();

            // multiply signal with gain per ear
            output
                .channel_data_mut(0)
                .iter_mut()
                .for_each(|v| *v *= gain_l * dist_gain * cone_gain);
            output
                .channel_data_mut(1)
                .iter_mut()
                .for_each(|v| *v *= gain_r * dist_gain * cone_gain);
        }

        false // only true for panning model HRTF
    }
}
