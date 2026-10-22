use sap_router::config::Config;

#[test]
fn yaml_defaults_and_validation() {
    let config: Config =
        serde_yaml_ng::from_str("servers:\n  - name: primary\n    base_url: http://127.0.0.1:8318\n").unwrap();
    config.validate().unwrap();
    assert_eq!(config.listen.to_string(), "127.0.0.1:8317");
    assert_eq!(config.probe_interval_ms, 1000);
    assert_eq!(config.probe_timeout_ms, 1500);
    assert_eq!(config.down_after_failures, 2);
    assert_eq!(config.stable_secs, 30);
    assert_eq!(config.connect_timeout_ms, 1000);
    assert!(!config.client_host.is_empty());
    for invalid in [
        "https://example.invalid",
        "http://user:secret@localhost",
        "http://localhost/prefix",
        "http://localhost?key=secret",
        "file:///tmp/server",
    ] {
        let mut config = config.clone();
        config.servers[0].base_url = invalid.into();
        assert!(config.validate().is_err());
    }
    let mut invalid = config.clone();
    invalid.servers.push(invalid.servers[0].clone());
    assert!(invalid.validate().is_err());
    invalid = config.clone();
    invalid.probe_interval_ms = 0;
    assert!(invalid.validate().is_err());
    invalid = config;
    invalid.client_host = "bad\r\nheader".into();
    assert!(invalid.validate().is_err());
    assert!(Config::default().validate().is_err());
    assert!(serde_yaml_ng::from_str::<Config>("typo: true").is_err());
}
