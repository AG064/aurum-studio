//! Bounded private-game monitors and scoped engine-reported viewport timers.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

pub(crate) const RECEIPT_BYTES: usize = 64 * 1024;
pub(crate) const MAX_SAMPLES: u64 = 120;
const REQUEST_BYTES: usize = 8 * 1024;
const CPU_SCOPE: &str = "Godot rolling-second process/physics maxima; repeated monitor reads, not individual frame timings";
const VIEWPORT_CPU_SCOPE: &str = "Main viewport latest available CPU render duration; excludes scripts, engine subsystems, other viewports and global frame setup";
const VIEWPORT_GPU_SCOPE: &str = "Main viewport latest available GPU render timestamp duration; asynchronous results may repeat or lag; other viewports and presentation excluded";
const MAX_POST_DRAW: u64 = 16_384;
const MAX_OBSERVATION_AGE_US: u64 = 250_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Metric {
    Process,
    Physics,
    Memory,
    Nodes,
    Resources,
    TextureMemory,
    DrawCalls,
    RenderedObjects,
    Gpu,
    Presentation,
    ViewportCpu,
    ViewportGpu,
}
const METRICS: [Metric; 12] = [
    Metric::Process,
    Metric::Physics,
    Metric::Memory,
    Metric::Nodes,
    Metric::Resources,
    Metric::TextureMemory,
    Metric::DrawCalls,
    Metric::RenderedObjects,
    Metric::Gpu,
    Metric::Presentation,
    Metric::ViewportCpu,
    Metric::ViewportGpu,
];
impl Metric {
    fn name(self) -> &'static str {
        match self {
            Self::Process => "process_peak_ms",
            Self::Physics => "physics_peak_ms",
            Self::Memory => "static_memory_bytes",
            Self::Nodes => "node_count",
            Self::Resources => "resource_count",
            Self::TextureMemory => "texture_memory_bytes",
            Self::DrawCalls => "draw_call_count",
            Self::RenderedObjects => "rendered_object_count",
            Self::Gpu => "gpu_frame_ms",
            Self::Presentation => "frame_presentation_ms",
            Self::ViewportCpu => "viewport_cpu_render_ms",
            Self::ViewportGpu => "gpu_viewport_ms",
        }
    }
    fn from_name(name: &str) -> Result<Self, String> {
        METRICS
            .into_iter()
            .find(|metric| metric.name() == name)
            .ok_or_else(|| "unsupported performance budget metric".into())
    }
    fn unit(self) -> &'static str {
        match self {
            Self::Process
            | Self::Physics
            | Self::Gpu
            | Self::Presentation
            | Self::ViewportCpu
            | Self::ViewportGpu => "ms",
            Self::Memory | Self::TextureMemory => "bytes",
            _ => "count",
        }
    }
    fn maximum(self) -> f64 {
        match self.unit() {
            "ms" => 60_000.0,
            "bytes" => (1u64 << 40) as f64,
            _ => 1_000_000_000.0,
        }
    }
    fn expected_state(self, runtime: &Runtime) -> &'static str {
        match self {
            Self::Gpu | Self::Presentation => "not_instrumented",
            _ if runtime.major != 4 || runtime.minor != 7 => "unsupported_runtime",
            Self::Memory if !runtime.debug => "requires_debug_build",
            Self::TextureMemory | Self::DrawCalls | Self::RenderedObjects if !runtime.rendered => {
                "headless_renderer"
            }
            Self::ViewportCpu | Self::ViewportGpu if !runtime.rendered => "headless_renderer",
            Self::ViewportCpu | Self::ViewportGpu => "timer_not_ready_or_unsupported",
            _ => "available",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub(crate) warmup_frames: u64,
    pub(crate) sample_frames: u64,
    pub(crate) sample_stride: u64,
    pub(crate) min_warmup_ms: u64,
    pub(crate) wall_budget_ms: u64,
    frame_budget: u64,
    timeout_seconds: u64,
    budgets: BTreeMap<Metric, f64>,
}
impl Request {
    pub(crate) fn parse(value: &Value, frames: u64, timeout_seconds: u64) -> Result<Self, String> {
        if !(1..=3_600_000).contains(&frames) || !(1..=600).contains(&timeout_seconds) {
            return Err("performance run bounds exceed playtest contract".into());
        }
        optional_fields(
            value,
            &[
                "warmup_frames",
                "sample_frames",
                "sample_stride",
                "min_warmup_ms",
                "wall_budget_ms",
                "budgets",
            ],
        )?;
        encode(value, REQUEST_BYTES)?;
        let warmup_frames = option_integer(value, "warmup_frames", 120, 1, 1200)?;
        let sample_frames = option_integer(value, "sample_frames", 120, 1, MAX_SAMPLES)?;
        let sample_stride = option_integer(value, "sample_stride", 1, 1, 60)?;
        let min_warmup_ms = option_integer(value, "min_warmup_ms", 1100, 1100, 5000)?;
        let wall_budget_ms = option_integer(value, "wall_budget_ms", 10000, 1101, 60000)?;
        if min_warmup_ms >= wall_budget_ms || wall_budget_ms > timeout_seconds.saturating_mul(1000)
        {
            return Err("performance wall window exceeds playtest timeout or warmup".into());
        }
        let final_frame = warmup_frames + 1 + (sample_frames - 1) * sample_stride;
        if frames < final_frame {
            return Err("performance sample window exceeds playtest frame budget".into());
        }
        let mut budgets = BTreeMap::new();
        if let Some(value) = value.get("budgets") {
            let object = value
                .as_object()
                .ok_or("performance budgets must be an object")?;
            if object.len() > METRICS.len() {
                return Err("performance budget count exceeded".into());
            }
            for (name, value) in object {
                let metric = Metric::from_name(name)?;
                let maximum = number(value, metric.maximum())?;
                if metric.unit() != "ms" && maximum.fract() != 0.0 {
                    return Err("memory and count budgets must be integral".into());
                }
                if metric.unit() == "ms" && maximum == 0.0 {
                    return Err("timing budgets must be positive".into());
                }
                budgets.insert(metric, maximum);
            }
        }
        Ok(Self {
            warmup_frames,
            sample_frames,
            sample_stride,
            min_warmup_ms,
            wall_budget_ms,
            frame_budget: frames,
            timeout_seconds,
            budgets,
        })
    }
    pub(crate) fn options(&self, request_id: &str) -> Result<Value, String> {
        identity(request_id)?;
        Ok(
            json!({"schema_version":1,"request_id":request_id,"frame_budget":self.frame_budget,
            "warmup_frames":self.warmup_frames,"sample_frames":self.sample_frames,
            "sample_stride":self.sample_stride,"min_warmup_ms":self.min_warmup_ms,
            "wall_budget_ms":self.wall_budget_ms}),
        )
    }
}

struct Runtime {
    pid: u64,
    major: u64,
    minor: u64,
    patch: u64,
    debug: bool,
    rendered: bool,
}
struct Sample {
    values: BTreeMap<Metric, Option<f64>>,
}

struct ViewportTiming<'a> {
    enabled: bool,
    method: &'a str,
    driver: &'a str,
    cpu: &'a str,
    gpu: &'a str,
    draws: u64,
}
impl<'a> ViewportTiming<'a> {
    fn parse(value: &'a Value, runtime: &Runtime) -> Result<Self, String> {
        fields(
            value,
            &[
                "scope",
                "backend",
                "enabled_during_sampling",
                "method",
                "driver",
                "cpu_state",
                "gpu_state",
                "post_draw_observations",
                "observation_read_age_limit_us",
                "subviewports_included",
                "presentation_included",
                "distinct_gpu_frame_ids_verified",
            ],
        )?;
        if value["scope"] != "main_viewport_only"
            || value["backend"] != "rendering_server_viewport_timer"
            || value["observation_read_age_limit_us"] != MAX_OBSERVATION_AGE_US
            || value["subviewports_included"] != false
            || value["presentation_included"] != false
            || value["distinct_gpu_frame_ids_verified"] != false
        {
            return Err("viewport timing scope or provenance refused".into());
        }
        let result = Self {
            enabled: value["enabled_during_sampling"]
                .as_bool()
                .ok_or("invalid viewport instrumentation flag")?,
            method: value["method"]
                .as_str()
                .ok_or("invalid viewport rendering method")?,
            driver: value["driver"]
                .as_str()
                .ok_or("invalid viewport rendering driver")?,
            cpu: value["cpu_state"]
                .as_str()
                .ok_or("invalid viewport CPU state")?,
            gpu: value["gpu_state"]
                .as_str()
                .ok_or("invalid viewport GPU state")?,
            draws: producer_integer(&value["post_draw_observations"], 0, MAX_POST_DRAW)?,
        };
        if !matches!(
            result.method,
            "unknown" | "headless" | "forward_plus" | "mobile" | "gl_compatibility"
        ) || !matches!(
            result.driver,
            "unknown"
                | "headless"
                | "vulkan"
                | "d3d12"
                | "metal"
                | "opengl3"
                | "opengl3_es"
                | "opengl3_angle"
        ) {
            return Err("uncurated viewport renderer identity refused".into());
        }
        let fixed = if runtime.major != 4 || runtime.minor != 7 {
            Some(("unsupported_runtime", "unknown"))
        } else if !runtime.rendered {
            Some(("headless_renderer", "headless"))
        } else {
            None
        };
        if let Some((state, name)) = fixed {
            if result.enabled
                || result.draws != 0
                || result.cpu != state
                || result.gpu != state
                || result.method != name
                || result.driver != name
            {
                return Err("viewport timing contradicts runtime capabilities".into());
            }
            return Ok(result);
        }
        let renderer_matches = match result.method {
            "gl_compatibility" => {
                matches!(result.driver, "opengl3" | "opengl3_es" | "opengl3_angle")
            }
            "forward_plus" | "mobile" => matches!(result.driver, "vulkan" | "d3d12" | "metal"),
            _ => false,
        };
        if !renderer_matches {
            if result.enabled
                || result.draws != 0
                || result.cpu != "unsupported_renderer"
                || result.gpu != "unsupported_renderer"
            {
                return Err("unsupported renderer claimed viewport timing".into());
            }
        } else if !result.enabled {
            if result.draws != 0
                || result.cpu != "viewport_unavailable"
                || result.gpu != "viewport_unavailable"
            {
                return Err("disabled viewport timer claimed measurements".into());
            }
        } else {
            for state in [result.cpu, result.gpu] {
                if !matches!(
                    state,
                    "available"
                        | "timer_not_ready_or_unsupported"
                        | "stale_viewport_observation"
                        | "invalid_viewport_timing"
                        | "observation_bound_exceeded"
                ) || state == "available" && result.draws < 2
                {
                    return Err("invalid viewport timing availability".into());
                }
            }
        }
        Ok(result)
    }
}
struct Writer {
    bytes: Vec<u8>,
    limit: usize,
}
impl Write for Writer {
    fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
        if value.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("performance byte bound exceeded"));
        }
        self.bytes.extend_from_slice(value);
        Ok(value.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn encode(value: &Value, maximum: usize) -> Result<Vec<u8>, String> {
    let mut writer = Writer {
        bytes: Vec::new(),
        limit: maximum,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| "performance byte bound exceeded")?;
    Ok(writer.bytes)
}
fn optional_fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("performance data must be an object")?;
    if object.len() > allowed.len() || object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("unknown performance field".into());
    }
    Ok(())
}
fn fields(value: &Value, allowed: &[&str]) -> Result<(), String> {
    optional_fields(value, allowed)?;
    if value.as_object().ok_or("performance object missing")?.len() != allowed.len() {
        return Err("missing performance field".into());
    }
    Ok(())
}
fn integer(value: &Value, minimum: u64, maximum: u64) -> Result<u64, String> {
    value
        .as_u64()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| "invalid performance integer".into())
}
fn producer_integer(value: &Value, minimum: u64, maximum: u64) -> Result<u64, String> {
    let value = number(value, maximum as f64)?;
    if value.fract() != 0.0 || value < minimum as f64 {
        return Err("invalid performance producer integer".into());
    }
    Ok(value as u64)
}
fn option_integer(
    value: &Value,
    key: &str,
    default: u64,
    min: u64,
    max: u64,
) -> Result<u64, String> {
    value
        .get(key)
        .map(|value| integer(value, min, max))
        .transpose()
        .map(|value| value.unwrap_or(default))
}
fn number(value: &Value, maximum: f64) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= maximum)
        .ok_or_else(|| "invalid finite performance value".into())
}
fn identity(value: &str) -> Result<(), String> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|value| value.is_ascii_digit() || (b'a'..=b'f').contains(&value))
    {
        return Err("invalid performance request identity".into());
    }
    Ok(())
}

pub(crate) fn evaluate(
    request: &Request,
    request_id: &str,
    producer: &Value,
) -> Result<Value, String> {
    identity(request_id)?;
    fields(
        producer,
        &[
            "schema_version",
            "request_id",
            "frame_budget",
            "warmup_frames",
            "sample_frames",
            "sample_stride",
            "min_warmup_ms",
            "wall_budget_ms",
            "complete",
            "reason",
            "elapsed_us",
            "frames_observed",
            "runtime",
            "availability",
            "viewport_timing",
            "samples",
        ],
    )?;
    encode(producer, RECEIPT_BYTES)?;
    let expected = request.options(request_id)?;
    for key in [
        "schema_version",
        "request_id",
        "frame_budget",
        "warmup_frames",
        "sample_frames",
        "sample_stride",
        "min_warmup_ms",
        "wall_budget_ms",
    ] {
        if producer[key] != expected[key] {
            return Err("performance receipt does not match request".into());
        }
    }
    let complete = producer["complete"]
        .as_bool()
        .ok_or("invalid performance completion")?;
    let reason = producer["reason"]
        .as_str()
        .ok_or("invalid performance reason")?;
    if !matches!(
        reason,
        "sample_window_complete"
            | "incomplete"
            | "game_exit"
            | "wall_budget_exhausted"
            | "warmup_wall_unmet"
            | "monitor_not_initialized"
            | "invalid_monitor_value"
            | "unsupported_runtime"
            | "output_budget_exceeded"
    ) || complete != (reason == "sample_window_complete")
    {
        return Err("inconsistent performance completion".into());
    }
    let elapsed = producer_integer(
        &producer["elapsed_us"],
        0,
        request.timeout_seconds * 1_000_000 + 5_000_000,
    )?;
    let observed = producer_integer(&producer["frames_observed"], 0, request.frame_budget)?;
    let runtime = &producer["runtime"];
    fields(
        runtime,
        &["pid", "major", "minor", "patch", "debug_build", "rendered"],
    )?;
    let runtime = Runtime {
        pid: producer_integer(&runtime["pid"], 1, u32::MAX as u64)?,
        major: producer_integer(&runtime["major"], 0, 99)?,
        minor: producer_integer(&runtime["minor"], 0, 99)?,
        patch: producer_integer(&runtime["patch"], 0, 999)?,
        debug: runtime["debug_build"]
            .as_bool()
            .ok_or("invalid runtime build flag")?,
        rendered: runtime["rendered"]
            .as_bool()
            .ok_or("invalid runtime rendering flag")?,
    };
    let viewport = ViewportTiming::parse(&producer["viewport_timing"], &runtime)?;
    fields(&producer["availability"], &METRICS.map(Metric::name))?;
    let mut availability = BTreeMap::new();
    for metric in METRICS {
        let state = producer["availability"][metric.name()]
            .as_str()
            .ok_or("invalid monitor availability")?;
        let expected = match metric {
            Metric::ViewportCpu => viewport.cpu,
            Metric::ViewportGpu => viewport.gpu,
            _ => metric.expected_state(&runtime),
        };
        if state != expected {
            return Err("monitor availability contradicts runtime capabilities".into());
        }
        availability.insert(metric, state);
    }
    let samples = producer["samples"]
        .as_array()
        .filter(|rows| rows.len() <= request.sample_frames as usize)
        .ok_or("performance sample bound exceeded")?;
    let mut parsed = Vec::with_capacity(samples.len());
    let mut previous_time = None;
    let mut previous_render_event = None;
    for (index, row) in samples.iter().enumerate() {
        fields(
            row,
            &["frame", "elapsed_us", "metrics", "viewport_observation"],
        )?;
        let frame = producer_integer(&row["frame"], 1, request.frame_budget)?;
        if frame != request.warmup_frames + 1 + index as u64 * request.sample_stride
            || frame > observed
        {
            return Err("performance frame cadence mismatch".into());
        }
        let time = producer_integer(
            &row["elapsed_us"],
            request.min_warmup_ms * 1000,
            request.wall_budget_ms * 1000,
        )?;
        if time > elapsed || previous_time.is_some_and(|previous| time <= previous) {
            return Err("performance time order mismatch".into());
        }
        previous_time = Some(time);
        let observation = &row["viewport_observation"];
        if viewport.cpu == "available" || viewport.gpu == "available" {
            fields(observation, &["event", "process_frame", "elapsed_us"])?;
            let event = producer_integer(&observation["event"], 2, viewport.draws)?;
            let cached_frame = producer_integer(&observation["process_frame"], 0, frame)?;
            let cached_time = producer_integer(&observation["elapsed_us"], 0, time)?;
            if previous_render_event.is_some_and(|previous| event <= previous)
                || cached_frame + 1 < frame
                || time - cached_time > MAX_OBSERVATION_AGE_US
            {
                return Err("stale or repeated viewport observation refused".into());
            }
            previous_render_event = Some(event);
        } else if !observation.is_null() {
            return Err("unavailable viewport timing must not claim an observation".into());
        }
        fields(&row["metrics"], &METRICS.map(Metric::name))?;
        let mut values = BTreeMap::new();
        for metric in METRICS {
            let value = &row["metrics"][metric.name()];
            let value = if availability[&metric] == "available" {
                let value = number(value, metric.maximum())?;
                if metric == Metric::Process && value == 0.0 {
                    return Err("process timing monitor is not initialized".into());
                }
                if matches!(metric, Metric::ViewportCpu | Metric::ViewportGpu) && value == 0.0 {
                    return Err("viewport timing is not initialized".into());
                }
                if metric.unit() != "ms" && value.fract() != 0.0 {
                    return Err("nonintegral monitor count or memory".into());
                }
                Some(value)
            } else {
                if !value.is_null() {
                    return Err("unavailable monitor must not contain a numeric value".into());
                }
                None
            };
            values.insert(metric, value);
        }
        parsed.push(Sample { values });
    }
    if complete
        && (samples.len() != request.sample_frames as usize
            || runtime.major != 4
            || runtime.minor != 7
            || elapsed > request.wall_budget_ms * 1000)
    {
        return Err("incomplete performance window claimed complete".into());
    }
    let mut measurements = serde_json::Map::new();
    let mut assessments = Vec::new();
    let mut failures = BTreeSet::new();
    if !complete {
        failures.insert(reason);
    }
    for metric in METRICS {
        let values: Vec<f64> = parsed
            .iter()
            .filter_map(|row| row.values[&metric])
            .collect();
        let peak = values.iter().copied().reduce(f64::max);
        let mut measured = if let Some(peak) = peak {
            json!({"status":"available","unit":metric.unit(),"samples":values.len(),"minimum":values.iter().copied().reduce(f64::min),"mean":values.iter().sum::<f64>()/values.len() as f64,"peak":peak})
        } else {
            json!({"status":"unavailable","unit":metric.unit(),"reason":if availability[&metric]=="available"{"no_samples"}else{availability[&metric]}})
        };
        if matches!(metric, Metric::ViewportCpu | Metric::ViewportGpu) {
            measured["scope"] = json!(if metric == Metric::ViewportCpu {
                VIEWPORT_CPU_SCOPE
            } else {
                VIEWPORT_GPU_SCOPE
            });
        }
        measurements.insert(metric.name().into(), measured);
        if let Some(limit) = request.budgets.get(&metric) {
            let verdict = match peak {
                None => "unverified",
                Some(peak) if peak > *limit => "exceeded",
                Some(_) if !complete => "incomplete",
                Some(_) => "passed",
            };
            if verdict != "passed" {
                failures.insert(verdict);
            }
            assessments.push(json!({"metric":metric.name(),"maximum":limit,"observed_peak":peak,"verdict":verdict}));
        }
    }
    let result = json!({"schema_version":1,"ok":failures.is_empty(),"measurement_complete":complete,"reason":reason,
        "request_id":request_id,"samples_observed":samples.len(),"frames_observed":observed,"elapsed_us":elapsed,
        "runtime":{"pid":runtime.pid,"major":runtime.major,"minor":runtime.minor,"patch":runtime.patch,"debug_build":runtime.debug,"rendered":runtime.rendered},
        "measurements":measurements,"budgets":assessments,"failures":failures,"cpu_scope":CPU_SCOPE,
        "sampling_scope":"fixed-frame reads after warmup; cached CPU windows may overlap warmup; includes observer and gameplay overhead",
        "provenance":"private game process reported Performance monitors and scoped RenderingServer timers; schema/budgets validated by host, not independent kernel profiling",
        "viewport_timing":{"scope":"main_viewport_only","method":viewport.method,"driver":viewport.driver,"enabled_during_sampling":viewport.enabled,
            "cpu_state":viewport.cpu,"gpu_state":viewport.gpu,"post_draw_observations":viewport.draws,"observation_read_age_limit_us":MAX_OBSERVATION_AGE_US,
            "gpu_scope":VIEWPORT_GPU_SCOPE,"cpu_scope":VIEWPORT_CPU_SCOPE,"subviewports_included":false,"total_scene_gpu_time_verified":false,
            "presentation_included":false,"distinct_gpu_frame_ids_verified":false,"instrumentation_overhead_included":true,
            "last_available_result":true,"gpu_support_independently_verified":false,"gpu_result_age_verified":false},
        "gpu_viewport_timer_observed":viewport.gpu=="available" && !parsed.is_empty(),
        "producer_sha256":crate::sha256_hex(&encode(producer,RECEIPT_BYTES)?),"raw_samples_returned":false,
        "gpu_timing_verified":false,"frame_presentation_verified":false,"physical_device_verified":false});
    encode(&result, RECEIPT_BYTES)?;
    Ok(result)
}

pub(crate) fn input_schema() -> Value {
    let mut budgets = serde_json::Map::new();
    for metric in METRICS {
        let mut property = json!({"type":if metric.unit()=="ms"{"number"}else{"integer"},"maximum":metric.maximum(),"description":"Maximum observed monitor value; unavailable requested metrics make the budget unverified"});
        property["description"] = json!(match metric {
            Metric::ViewportCpu => VIEWPORT_CPU_SCOPE,
            Metric::ViewportGpu => VIEWPORT_GPU_SCOPE,
            Metric::Gpu => "Whole-scene/full-frame GPU timing is not instrumented; use the explicitly scoped gpu_viewport_ms separately",
            _ => "Maximum observed monitor value; unavailable requested metrics make the budget unverified",
        });
        if metric.unit() == "ms" {
            property["exclusiveMinimum"] = json!(0);
        } else {
            property["minimum"] = json!(0);
        }
        budgets.insert(metric.name().into(), property);
    }
    json!({"type":"object","additionalProperties":false,"description":"Opt-in private Windows monitor sampling with scoped rendered viewport CPU/GPU timers; whole-scene GPU and presentation timings unavailable; does not qualify gameplay or a device",
        "properties":{"warmup_frames":{"type":"integer","minimum":1,"maximum":1200,"default":120},"sample_frames":{"type":"integer","minimum":1,"maximum":MAX_SAMPLES,"default":120},"sample_stride":{"type":"integer","minimum":1,"maximum":60,"default":1},"min_warmup_ms":{"type":"integer","minimum":1100,"maximum":5000,"default":1100},"wall_budget_ms":{"type":"integer","minimum":1101,"maximum":60000,"default":10000},"budgets":{"type":"object","additionalProperties":false,"maxProperties":METRICS.len(),"properties":budgets}},
        "measurement_semantics":CPU_SCOPE,"viewport_timing_semantics":{"scope":"main_viewport_only","gpu":VIEWPORT_GPU_SCOPE,"cpu":VIEWPORT_CPU_SCOPE,"total_scene_gpu_time_verified":false,"frame_presentation_verified":false}})
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "0123456789abcdef0123456789abcdef";
    fn request(budgets: Value) -> Request {
        Request::parse(
            &json!({"warmup_frames":2,"sample_frames":3,"budgets":budgets}),
            5,
            90,
        )
        .unwrap()
    }
    fn producer(request: &Request, debug: bool, rendered: bool) -> Value {
        let runtime = Runtime {
            pid: 1234,
            major: 4,
            minor: 7,
            patch: 0,
            debug,
            rendered,
        };
        let mut availability = serde_json::Map::new();
        let mut samples = Vec::new();
        for metric in METRICS {
            availability.insert(metric.name().into(), json!(metric.expected_state(&runtime)));
        }
        for index in 0..request.sample_frames {
            let mut metrics = serde_json::Map::new();
            for metric in METRICS {
                let value = if metric.expected_state(&runtime) != "available" {
                    Value::Null
                } else {
                    json!(match metric {
                        Metric::Process => 2.0 + index as f64,
                        Metric::Physics => 1.0,
                        Metric::Memory => 1048576.0,
                        Metric::Nodes => 40.0 + index as f64,
                        Metric::Resources => 8.0,
                        Metric::TextureMemory => 8192.0,
                        Metric::DrawCalls => 7.0,
                        Metric::RenderedObjects => 12.0,
                        _ => unreachable!(),
                    })
                };
                metrics.insert(metric.name().into(), value);
            }
            samples.push(json!({"frame":request.warmup_frames+1+index*request.sample_stride,"elapsed_us":1_200_000+index*100_000,"metrics":metrics,"viewport_observation":null}));
        }
        let mut result = request.options(ID).unwrap();
        result["complete"] = json!(true);
        result["reason"] = json!("sample_window_complete");
        result["elapsed_us"] = json!(1_400_000);
        result["frames_observed"] = json!(5);
        result["runtime"] = json!({"pid":1234,"major":4,"minor":7,"patch":0,"debug_build":debug,"rendered":rendered});
        result["availability"] = Value::Object(availability);
        result["viewport_timing"] = json!({"scope":"main_viewport_only","backend":"rendering_server_viewport_timer","enabled_during_sampling":rendered,
            "method":if rendered{"gl_compatibility"}else{"headless"},"driver":if rendered{"opengl3"}else{"headless"},
            "cpu_state":Metric::ViewportCpu.expected_state(&runtime),"gpu_state":Metric::ViewportGpu.expected_state(&runtime),
            "post_draw_observations":if rendered{5}else{0},"observation_read_age_limit_us":MAX_OBSERVATION_AGE_US,
            "subviewports_included":false,"presentation_included":false,"distinct_gpu_frame_ids_verified":false});
        result["samples"] = json!(samples);
        result
    }

    fn viewport_producer(request: &Request) -> Value {
        let mut record = producer(request, true, true);
        for (metric, state_key) in [
            ("viewport_cpu_render_ms", "cpu_state"),
            ("gpu_viewport_ms", "gpu_state"),
        ] {
            record["availability"][metric] = json!("available");
            record["viewport_timing"][state_key] = json!("available");
        }
        for (index, row) in record["samples"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            row["metrics"]["viewport_cpu_render_ms"] = json!(0.5 + index as f64 * 0.1);
            row["metrics"]["gpu_viewport_ms"] = json!(2.0 + index as f64);
            row["viewport_observation"] = json!({"event":index+3,"process_frame":row["frame"].as_u64().unwrap()-1,"elapsed_us":row["elapsed_us"].as_u64().unwrap()-1000});
        }
        record
    }

    #[test]
    fn scoped_viewport_timers_pass_and_exceed_without_whole_frame_claim() {
        let accepted = request(json!({"gpu_viewport_ms":4.0,"viewport_cpu_render_ms":0.8}));
        let report = evaluate(&accepted, ID, &viewport_producer(&accepted)).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["measurements"]["gpu_viewport_ms"]["peak"], 4.0);
        assert_eq!(report["gpu_viewport_timer_observed"], true);
        assert_eq!(report["gpu_timing_verified"], false);
        assert_eq!(
            report["viewport_timing"]["total_scene_gpu_time_verified"],
            false
        );
        assert_eq!(report["viewport_timing"]["subviewports_included"], false);
        assert_eq!(
            report["viewport_timing"]["distinct_gpu_frame_ids_verified"],
            false
        );
        assert!(report["cpu_scope"]
            .as_str()
            .unwrap()
            .contains("rolling-second"));
        assert!(report["measurements"]["viewport_cpu_render_ms"]["scope"]
            .as_str()
            .unwrap()
            .contains("excludes scripts"));
        let exceeded = request(json!({"gpu_viewport_ms":3.9}));
        let report = evaluate(&exceeded, ID, &viewport_producer(&exceeded)).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["budgets"][0]["verdict"], "exceeded");
        let whole_frame = request(json!({"gpu_frame_ms":60_000.0,"gpu_viewport_ms":4.0}));
        let report = evaluate(&whole_frame, ID, &viewport_producer(&whole_frame)).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(
            report["measurements"]["gpu_frame_ms"]["status"],
            "unavailable"
        );
        assert!(report["budgets"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["metric"] == "gpu_frame_ms" && row["verdict"] == "unverified"));
    }

    #[test]
    fn viewport_zero_negative_nonfinite_and_stale_values_fail_closed() {
        let request = request(json!({"gpu_viewport_ms":20.0}));
        let record = viewport_producer(&request);
        for value in [
            json!(0),
            json!(-0.1),
            json!(60_001),
            json!(1e308),
            Value::Null,
        ] {
            let mut changed = record.clone();
            changed["samples"][0]["metrics"]["gpu_viewport_ms"] = value;
            assert!(evaluate(&request, ID, &changed).is_err());
        }
        for (index, key, value) in [
            (1, "event", json!(3)),
            (0, "process_frame", json!(0)),
            (0, "elapsed_us", json!(900_000)),
        ] {
            let mut changed = record.clone();
            changed["samples"][index]["viewport_observation"][key] = value;
            assert!(evaluate(&request, ID, &changed).is_err());
        }
    }

    #[test]
    fn unavailable_and_incomplete_viewport_windows_never_pass_budgets() {
        let request = request(json!({"gpu_viewport_ms":20.0,"viewport_cpu_render_ms":20.0}));
        for reason in [
            "timer_not_ready_or_unsupported",
            "stale_viewport_observation",
            "invalid_viewport_timing",
            "observation_bound_exceeded",
        ] {
            let mut record = producer(&request, true, true);
            for (metric, key) in [
                ("gpu_viewport_ms", "gpu_state"),
                ("viewport_cpu_render_ms", "cpu_state"),
            ] {
                record["viewport_timing"][key] = json!(reason);
                record["availability"][metric] = json!(reason);
            }
            let report = evaluate(&request, ID, &record).unwrap();
            assert_eq!(report["ok"], false);
            assert_eq!(report["gpu_viewport_timer_observed"], false);
            assert!(report["budgets"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["verdict"] == "unverified" && row["observed_peak"].is_null()));
        }
        let report = evaluate(&request, ID, &producer(&request, true, false)).unwrap();
        assert_eq!(
            report["measurements"]["gpu_viewport_ms"]["reason"],
            "headless_renderer"
        );
        let mut record = viewport_producer(&request);
        record["complete"] = json!(false);
        record["reason"] = json!("game_exit");
        record["samples"].as_array_mut().unwrap().pop();
        let report = evaluate(&request, ID, &record).unwrap();
        assert_eq!(report["measurement_complete"], false);
        assert!(report["budgets"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["verdict"] == "incomplete"));
    }

    #[test]
    fn viewport_scope_capability_and_metadata_injection_refuse() {
        let request = request(json!({"gpu_viewport_ms":20.0}));
        let record = viewport_producer(&request);
        for (key, value) in [
            ("scope", json!("all_viewports")),
            ("subviewports_included", json!(true)),
            ("presentation_included", json!(true)),
            ("distinct_gpu_frame_ids_verified", json!(true)),
            ("enabled_during_sampling", json!(false)),
            ("post_draw_observations", json!(1)),
            ("method", json!("PRIVATE-GPU-NAME")),
            ("observation_read_age_limit_us", json!(1_000_000)),
        ] {
            let mut changed = record.clone();
            changed["viewport_timing"][key] = value;
            let error = evaluate(&request, ID, &changed).unwrap_err();
            assert!(!error.contains("PRIVATE-GPU-NAME"));
        }
        let mut changed = record.clone();
        changed["runtime"]["rendered"] = json!(false);
        assert!(evaluate(&request, ID, &changed).is_err());
        let mut changed = record;
        changed["viewport_timing"]["source_body"] = json!("PRIVATE");
        assert!(evaluate(&request, ID, &changed).is_err());
    }

    #[test]
    fn viewport_schema_distinguishes_scoped_metrics_from_full_frame() {
        let schema = input_schema();
        let budgets = &schema["properties"]["budgets"]["properties"];
        assert!(budgets["gpu_viewport_ms"]["description"]
            .as_str()
            .unwrap()
            .contains("Main viewport"));
        assert!(budgets["viewport_cpu_render_ms"]["description"]
            .as_str()
            .unwrap()
            .contains("excludes scripts"));
        assert!(budgets["gpu_frame_ms"]["description"]
            .as_str()
            .unwrap()
            .contains("not instrumented"));
        assert_eq!(
            schema["viewport_timing_semantics"]["total_scene_gpu_time_verified"],
            false
        );
        assert_eq!(
            schema["viewport_timing_semantics"]["frame_presentation_verified"],
            false
        );
    }
    #[test]
    fn observed_peak_budgets_pass_or_fail_without_individual_frame_claims() {
        let accepted = request(json!({"process_peak_ms":4.0,"node_count":42}));
        let report = evaluate(&accepted, ID, &producer(&accepted, true, false)).unwrap();
        assert_eq!(report["ok"], true);
        assert_eq!(report["measurements"]["process_peak_ms"]["mean"], 3.0);
        assert_eq!(report["measurements"]["process_peak_ms"]["peak"], 4.0);
        assert_eq!(report["gpu_timing_verified"], false);
        assert!(report["cpu_scope"]
            .as_str()
            .unwrap()
            .contains("rolling-second"));
        let refused = request(json!({"process_peak_ms":3.9}));
        let report = evaluate(&refused, ID, &producer(&refused, true, false)).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["budgets"][0]["verdict"], "exceeded");
        assert_eq!(report["measurement_complete"], true);
    }
    #[test]
    fn unavailable_requested_gpu_and_presentation_budgets_are_unverified() {
        let request = request(json!({"gpu_frame_ms":16.7,"frame_presentation_ms":20.0}));
        let report = evaluate(&request, ID, &producer(&request, true, true)).unwrap();
        assert_eq!(report["ok"], false);
        for budget in report["budgets"].as_array().unwrap() {
            assert_eq!(budget["verdict"], "unverified");
            assert_eq!(budget["observed_peak"], Value::Null);
        }
        assert!(report["measurements"]["gpu_frame_ms"].get("peak").is_none());
        assert_eq!(report["frame_presentation_verified"], false);
    }
    #[test]
    fn release_and_headless_monitor_availability_cannot_be_numeric_zero() {
        let request = request(json!({"static_memory_bytes":1_000_000,"draw_call_count":1}));
        let mut record = producer(&request, false, false);
        let report = evaluate(&request, ID, &record).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(
            report["measurements"]["static_memory_bytes"]["reason"],
            "requires_debug_build"
        );
        assert_eq!(
            report["measurements"]["draw_call_count"]["reason"],
            "headless_renderer"
        );
        record["samples"][0]["metrics"]["static_memory_bytes"] = json!(0);
        assert!(evaluate(&request, ID, &record).is_err());
    }
    #[test]
    fn closed_request_unknown_budget_and_wrong_numeric_units_refuse() {
        for input in [
            json!({"unknown":"private body"}),
            json!({"budgets":{"unknown_gpu_counter":1}}),
            json!({"budgets":{"process_peak_ms":0}}),
            json!({"budgets":{"node_count":1.5}}),
            json!({"sample_frames":121}),
            json!({"sample_stride":0}),
            json!({"warmup_frames":1.5}),
            json!({"min_warmup_ms":0}),
            json!({"wall_budget_ms":1100}),
            json!({"budgets":{"static_memory_bytes":-1}}),
        ] {
            let error = Request::parse(&input, 1000, 90).unwrap_err();
            assert!(!error.contains("private body"));
        }
        assert!(Request::parse(&json!({}), 239, 90).is_err());
        assert!(Request::parse(&json!({"wall_budget_ms":10000}), 240, 9).is_err());
        assert!(Request::parse(&json!({}), 3_600_001, 90).is_err());
        assert!(Request::parse(&json!({}), 240, 601).is_err());
        assert!(Request::parse(&json!({"budgets":{"node_count":0}}), 240, 90).is_ok());
    }
    #[test]
    fn producer_identity_unknown_body_and_false_complete_refuse() {
        let request = request(json!({}));
        let record = producer(&request, true, false);
        let mut changed = record.clone();
        changed["request_id"] = json!("f".repeat(32));
        assert!(evaluate(&request, ID, &changed).is_err());
        let mut changed = record.clone();
        changed["source_body"] = json!("PRIVATE".repeat(12000));
        assert!(evaluate(&request, ID, &changed).is_err());
        let mut changed = record.clone();
        changed["samples"].as_array_mut().unwrap().pop();
        assert!(evaluate(&request, ID, &changed).is_err());
        let mut changed = record.clone();
        changed["availability"]["gpu_frame_ms"] = json!("available");
        assert!(evaluate(&request, ID, &changed).is_err());
        let mut changed = record.clone();
        changed["runtime"]["pid"] = json!(0);
        assert!(evaluate(&request, ID, &changed).is_err());
    }
    #[test]
    fn cadence_time_order_warmup_and_finite_values_are_bound() {
        let request = request(json!({}));
        let record = producer(&request, true, false);
        for (index, key, value) in [
            (1, "frame", json!(5)),
            (0, "elapsed_us", json!(1_000_000)),
            (1, "elapsed_us", json!(1_200_000)),
        ] {
            let mut changed = record.clone();
            changed["samples"][index][key] = value;
            assert!(evaluate(&request, ID, &changed).is_err());
        }
        for (key, value) in [
            ("process_peak_ms", json!(0)),
            ("physics_peak_ms", json!(-1)),
            ("node_count", json!(1.5)),
            ("process_peak_ms", json!(60001)),
            ("gpu_frame_ms", json!(0)),
        ] {
            let mut changed = record.clone();
            changed["samples"][0]["metrics"][key] = value;
            assert!(evaluate(&request, ID, &changed).is_err());
        }
    }
    #[test]
    fn partial_wall_timeout_is_failure_with_no_clean_budget_pass() {
        let request = request(json!({"process_peak_ms":10}));
        let mut record = producer(&request, true, false);
        record["samples"].as_array_mut().unwrap().pop();
        record["complete"] = json!(false);
        record["reason"] = json!("wall_budget_exhausted");
        record["elapsed_us"] = json!(15_000_000);
        record["frames_observed"] = json!(4);
        let report = evaluate(&request, ID, &record).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(report["budgets"][0]["verdict"], "incomplete");
        assert_eq!(report["samples_observed"], 2);
        assert_eq!(report["elapsed_us"], 15_000_000);
    }
    #[test]
    fn unknown_runtime_is_incomplete_without_borrowing47_capabilities() {
        let request = request(json!({}));
        let mut record = producer(&request, true, false);
        record["runtime"]["minor"] = json!(8);
        record["complete"] = json!(false);
        record["reason"] = json!("unsupported_runtime");
        record["elapsed_us"] = json!(0);
        record["frames_observed"] = json!(0);
        record["samples"] = json!([]);
        record["viewport_timing"]["method"] = json!("unknown");
        record["viewport_timing"]["driver"] = json!("unknown");
        record["viewport_timing"]["cpu_state"] = json!("unsupported_runtime");
        record["viewport_timing"]["gpu_state"] = json!("unsupported_runtime");
        for metric in METRICS {
            if !matches!(metric, Metric::Gpu | Metric::Presentation) {
                record["availability"][metric.name()] = json!("unsupported_runtime");
            }
        }
        let report = evaluate(&request, ID, &record).unwrap();
        assert_eq!(report["ok"], false);
        assert_eq!(
            report["measurements"]["process_peak_ms"]["reason"],
            "unsupported_runtime"
        );
    }
    #[test]
    fn bounded_json_encoding_counts_escaped_bytes_and_schema_units_match() {
        assert_eq!(
            encode(&json!("x".repeat(RECEIPT_BYTES - 2)), RECEIPT_BYTES)
                .unwrap()
                .len(),
            RECEIPT_BYTES
        );
        assert!(encode(&json!("x".repeat(RECEIPT_BYTES - 1)), RECEIPT_BYTES).is_err());
        assert!(encode(&json!("\n".repeat(RECEIPT_BYTES / 2)), RECEIPT_BYTES).is_err());
        let schema = input_schema();
        assert_eq!(schema["additionalProperties"], false);
        let fields = schema["properties"]["budgets"]["properties"]
            .as_object()
            .unwrap();
        assert_eq!(fields.len(), METRICS.len());
        for metric in METRICS {
            assert!(fields.contains_key(metric.name()));
        }
        assert_eq!(fields["process_peak_ms"]["exclusiveMinimum"], 0);
        assert_eq!(fields["node_count"]["minimum"], 0);
    }
}
