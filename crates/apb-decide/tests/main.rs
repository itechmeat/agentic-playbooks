//! The one integration test binary of apb-decide (docs/TESTING-GUIDELINES.md).

#[path = "suite/chain_test.rs"]
mod chain_test;
#[path = "suite/llm_emulation_test.rs"]
mod llm_emulation_test;
#[path = "suite/systemone_test.rs"]
mod systemone_test;
