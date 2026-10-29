//#include pixel_storage
//#include layer_spill_head

@compute @workgroup_size(1)
fn spill_reset() {
    atomicStore(&region.x0, i32(params.width));
    atomicStore(&region.y0, i32(params.height));
    atomicStore(&region.x1, 0);
    atomicStore(&region.y1, 0);
}
