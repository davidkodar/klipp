//! A test that needs a GPU and finds none. On a machine without one the
//! test says so and returns, its GPU half unverified; with
//! `KLIPP_TEST_REQUIRE_GPU=1` it fails instead, so a run that is meant to
//! check the GPU can't pass by skipping it.

/// Whether this run asks for the GPU (`KLIPP_TEST_REQUIRE_GPU=1`).
pub fn required() -> bool {
    std::env::var("KLIPP_TEST_REQUIRE_GPU").as_deref() == Ok("1")
}

/// The GPU this test needed isn't there, `why`: said and left
/// unverified, or a failure when the run requires the GPU.
#[track_caller]
pub fn missing(why: &str) {
    assert!(!required(), "KLIPP_TEST_REQUIRE_GPU=1, but {why}");
    eprintln!("UNVERIFIED: {why}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_gpu_fails_only_when_the_run_requires_one() {
        // Both halves in one test: the variable is the process's.
        std::env::remove_var("KLIPP_TEST_REQUIRE_GPU");
        assert!(!super::required());
        super::missing("no GPU here");
        std::env::set_var("KLIPP_TEST_REQUIRE_GPU", "1");
        assert!(super::required());
        let failed = std::panic::catch_unwind(|| super::missing("no GPU here")).is_err();
        std::env::remove_var("KLIPP_TEST_REQUIRE_GPU");
        assert!(failed, "a required GPU that is missing fails the test");
    }
}
