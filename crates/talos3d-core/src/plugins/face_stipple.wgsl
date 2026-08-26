// Screen-space stipple fill for the selected face.
//
// The raster is anchored to the framebuffer, not to the surface, so the dot
// pitch stays constant as the model is orbited or zoomed — the same way a
// selected face reads in SketchUp. Doing it in the fragment stage keeps the
// whole highlight at one mesh, one draw call, independent of how large or how
// tessellated the face is.

#import bevy_pbr::forward_io::VertexOutput

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> stipple_color: vec4<f32>;
// x = dot pitch in physical pixels, y = dot size in physical pixels.
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var<uniform> stipple_params: vec4<f32>;

@fragment
fn fragment(in: VertexOutput) -> @location(0) vec4<f32> {
    let pitch = max(stipple_params.x, 2.0);
    let dot_size = clamp(stipple_params.y, 1.0, pitch - 1.0);

    // `in.position` is the fragment coordinate in physical pixels.
    let pixel = floor(in.position.xy);
    let cell = pixel - floor(pixel / pitch) * pitch;
    if (cell.x >= dot_size || cell.y >= dot_size) {
        discard;
    }

    return stipple_color;
}
