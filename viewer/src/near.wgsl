// Terrain material = StandardMaterial + a sharp (~1 m/px) aerial image around the truck.
// The near image's UVs come from world X/Z through a 2x3 affine map computed on the CPU
// (Mercator vs our Albers world is affine to well under a pixel over ~1.5 km).

#import bevy_pbr::{
    pbr_fragment::pbr_input_from_standard_material,
    pbr_functions::alpha_discard,
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
    // uv.x = u.x * x + u.y * z + u.z; u.w = 1 when enabled.
    u: vec4<f32>,
    // uv.y = v.x * x + v.y * z + v.z; v.w = edge fade width in UV units.
    v: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(100) var<uniform> near: NearImagery;
@group(#{MATERIAL_BIND_GROUP}) @binding(101) var near_texture: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(102) var near_sampler: sampler;

@fragment
fn fragment(
    in: VertexOutput,
    @builtin(front_facing) is_front: bool,
) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);

    let p = in.world_position.xz;
    let uv = vec2<f32>(near.u.x * p.x + near.u.y * p.y + near.u.z, near.v.x * p.x + near.v.y * p.y + near.v.z);
    // Sample unconditionally (texture sampling must be in uniform control flow).
    let c = textureSample(near_texture, near_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));
    let edge = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    let w = near.u.w * clamp(edge / max(near.v.w, 1e-4), 0.0, 1.0);
    pbr_input.material.base_color = vec4<f32>(mix(pbr_input.material.base_color.rgb, c.rgb, w), pbr_input.material.base_color.a);

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
