// Task lifecycle helpers (P3-003..P3-007).
// Core logic is in mapping/tasks.rs; this re-exports.

pub use crate::mapping::tasks::{
    map_task_create, map_task_get, map_task_list, map_task_stop, map_task_update,
    TaskCreateRequest, TaskCreateResult,
};

