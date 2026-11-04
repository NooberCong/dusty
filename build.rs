fn main() {
    println!("cargo:rerun-if-changed=res");
    embed_resource::compile("res/dusty.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
}
