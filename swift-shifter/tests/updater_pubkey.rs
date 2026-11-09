//! The key updates are checked against: v2.0.0's, minisign key ID
//! ACE67612BE281E4C. Changing it is changing who can ship an update, so a
//! change here should be as deliberate as one in `tauri.conf.json`.

/// `plugins.updater.pubkey` in `tauri.conf.json`, as Tauri reads it: the
/// minisign public key file, base64.
const PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEFDRTY3NjEyQkUyODFFNEMKUldSTUhpaStFbmJtckY5Q3pkK0dMZ0pZcUEyU0EzVS9KNHlBZkpVQmppVSt1dGtmdXd3Tyt2cUcK";

#[test]
fn the_updater_checks_updates_against_v2s_key() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json");
    let config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(config["plugins"]["updater"]["pubkey"], PUBKEY);
}
