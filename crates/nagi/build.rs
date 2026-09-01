fn main() {
    println!("cargo:rerun-if-env-changed=NAGI_CONTRACT_BUILD_REVISION");

    let Ok(revision) = std::env::var("NAGI_CONTRACT_BUILD_REVISION") else {
        return;
    };
    if revision.len() == 40
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        println!("cargo:rustc-env=NAGI_CONTRACT_BUILD_REVISION={revision}");
    }
}
