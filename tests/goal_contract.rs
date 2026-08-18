use std::fs;

#[test]
fn reviewed_user_task_contract_is_checked_in() {
    let reference = fs::read_to_string("docs/reference/user-task-and-goal-contract.md")
        .expect("task and goal reference exists");
    let fixture = fs::read_to_string("tests/fixtures/task/babysit_pr.submit.json")
        .expect("canonical submission fixture exists");

    assert!(reference.contains("GoalContract is immutable"));
    assert!(reference.contains("Slice 2 owns goal phases"));
    let value: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    assert_eq!(value["target"]["type"], "github_pull_request");
    assert_eq!(value["goal"]["success"].as_array().unwrap().len(), 3);
    assert!(value.get("principal").is_none());
}
