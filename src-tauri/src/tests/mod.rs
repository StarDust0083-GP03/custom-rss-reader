// Allow dead code in tests — utility functions are defined for future use.
#![allow(dead_code)]

pub mod chroma_e2e;
pub mod feed_item_tests;
pub mod ingest_e2e_tests;
pub mod ipc_contract_tests;
pub mod job_pipeline_tests;
pub mod helpers;
pub mod sqlite_compat_tests;
pub mod subscription_tests;
pub mod tag_matcher_tests;
pub mod topic_tests;
