//! exe にアイコン（assets/sqltrace.ico）を埋め込む。
fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    sqltrace_build::embed_icon(&root.join("assets").join("sqltrace.ico"));
}
