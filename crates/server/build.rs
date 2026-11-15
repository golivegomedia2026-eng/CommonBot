//! 界面在 `web/dist`，编译时嵌入二进制。还没构建界面时放一个占位页，
//! 这样只改 Rust 也能编译和跑测试；正式包必须先执行 `pnpm --dir web build`。

use std::path::Path;

fn main() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");
    println!("cargo:rerun-if-changed={}", dist.display());
    let index = dist.join("index.html");
    if !index.exists() {
        std::fs::create_dir_all(&dist).expect("create web/dist");
        std::fs::write(
            &index,
            "<!doctype html><meta charset=\"utf-8\"><title>CommonBot</title><p>界面尚未构建。请先运行 <code>pnpm --dir web install &amp;&amp; pnpm --dir web build</code>，再重新编译。</p>",
        )
        .expect("write placeholder");
    }
}
