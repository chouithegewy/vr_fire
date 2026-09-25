// Terrain material = StandardMaterial + two close-range layers:
//
// 1. A sharp (~0.6 m/px) NAIP aerial image around the truck, placed with an exact
//    world X/Z -> UV map (the image is exported in the same EPSG:5070 grid as the world).
// 2. Ground detail textures (CC0 grass, shrub soil, timber litter, rock, asphalt), chosen per
//    ~30 m cell from the LANDFIRE FBFM40 fuel-model map, modulating the aerial colour's
//    brightness within ~90 m of the camera.

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
    mesh_view_bindings::view,
}

#ifdef PREPASS_PIPELINE
#import bevy_pbr::{
    prepass_io::{VertexOutput, FragmentOutput},
    pbr_deferred_functions::deferred_output,
}
#else
#import bevy_pbr::{
    forward_io::{VertexOutput, FragmentOutput},
    pbr_functions::{apply_pbr_lighting, main_pass_post_lighting_processing},
}
#endif

struct NearImagery {
    // uv.x = u.x * x + u.y * z + u.z; u.w = 1 when the near image is enabled.
    u: vec4<f32>,
    // uv.y = v.x * x + v.y * z + v.z; v.w = edge fade width in UV units.
    v: vec4<f32>,
    // x = detail enabled, y = metres per repeat, z = strength, w = fade-out distance (m).
    detail: vec4<f32>,
    // Mean linear luminance of detail layers 0-3, and 4.
    lum_a: vec4<f32>,
    lum_b: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> near: NearImagery;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var near_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var near_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(103) var detail_texture: texture_2d_array<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(104) var detail_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(105) var fuel_texture: texture_2d<f32>;

fn hash2(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

// Smooth value noise in [0, 1].
fn value_noise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let s = f * f * (3.0 - 2.0 * f);
    let a = hash2(i);
    let b = hash2(i + vec2<f32>(1.0, 0.0));
    let c = hash2(i + vec2<f32>(0.0, 1.0));
    let d = hash2(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

fn layer_lum(layer: i32) -> f32 {
    var l = near.lum_b.x;
    if layer == 0 { l = near.lum_a.x; }
    if layer == 1 { l = near.lum_a.y; }
    if layer == 2 { l = near.lum_a.z; }
    if layer == 3 { l = near.lum_a.w; }
    return max(l, 1e-3);
}

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let p = in.world_position.xz;
    let uv = vec2<f32>(near.u.x * p.x + near.u.y * p.y + near.u.z, near.v.x * p.x + near.v.y * p.y + near.v.z);
    let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));

    // 1. Near aerial image. Sample unconditionally (texture sampling needs uniform control flow).
    let c = textureSample(near_texture, near_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    let w = near.u.w * clamp(edge / max(near.v.w, 1e-4), 0.0, 1.0);
    var base = mix(pbr_input.material.base_color.rgb, c.rgb, w);

    // 2. Ground detail. Fuel cell looked up with a world-space jitter so ~30 m cells don't read as squares.
    let jitter = (vec2<f32>(value_noise(p / 17.0), value_noise(p / 17.0 + vec2<f32>(5.2, 1.3))) - 0.5) * 24.0;
    let fuel_size = vec2<f32>(textureDimensions(fuel_texture));
    let fuv = clamp(uv + jitter * vec2<f32>(near.u.x, near.v.y), vec2<f32>(0.0), vec2<f32>(0.9999));
    let fuel_class = i32(round(textureLoad(fuel_texture, vec2<i32>(fuv * fuel_size), 0).r * 255.0));
    let layer = clamp(fuel_class, 0, 4);
    let tile = p / near.detail.y;
    // Two scales, blended, to hide tiling.
    let d1 = textureSample(detail_texture, detail_sampler, tile, layer).rgb;
    let d2 = textureSample(detail_texture, detail_sampler, tile * 0.27 + vec2<f32>(0.37, 0.71), layer).rgb;
    let d = mix(d1, d2, 0.4);
    let factor = dot(d, vec3<f32>(0.2126, 0.7152, 0.0722)) / layer_lum(layer);
    let dist = distance(view.world_position.xyz, in.world_position.xyz);
    let near_cam = 1.0 - smoothstep(near.detail.w * 0.5, near.detail.w, dist);
    let valid = select(0.0, 1.0, fuel_class < 5 && edge > 0.0);
    let k = near.detail.x * near.detail.z * near_cam * valid;
    base = base * mix(1.0, clamp(factor, 0.2, 2.5), k);

    pbr_input.material.base_color = vec4<f32>(base, pbr_input.material.base_color.a);
    pbr_input.material.base_color = alpha_discard(pbr_input.material, pbr_input.material.base_color);

#ifdef PREPASS_PIPELINE
    let out = deferred_output(in, pbr_input);
#else
    var out: FragmentOutput;
    out.color = apply_pbr_lighting(pbr_input);
    out.color = main_pass_post_lighting_processing(pbr_input, out.color);
#endif
    return out;
}
