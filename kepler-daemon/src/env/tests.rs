use super::*;

#[test]
fn test_insert_env_entries() {
    let mut env = HashMap::new();
    let entries = vec![
        "FOO=bar".to_string(),
        "BAZ=qux".to_string(),
        "INVALID".to_string(), // no '=' — should be skipped
    ];
    insert_env_entries(&mut env, &entries);
    assert_eq!(env.get("FOO").unwrap(), "bar");
    assert_eq!(env.get("BAZ").unwrap(), "qux");
    assert!(!env.contains_key("INVALID"));
}
