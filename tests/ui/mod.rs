#[cfg(feature = "multi-user")]
mod openid_connect_mock;

use tokio::task;

use krill::constants::*;
use krill::daemon::config::Config;
use krill::daemon::http::server;
use krill::test::*;

use std::env;
use std::path::PathBuf;
use std::process::Command;

pub async fn run_krill_ui_test(test_name: &str, _with_openid_server: bool) {
    #[cfg(feature = "multi-user")]
    let mock_server_join_handle = if _with_openid_server {
        openid_connect_mock::start().await
    } else {
        None
    };

    do_run_krill_ui_test(test_name).await;

    #[cfg(feature = "multi-user")]
    if _with_openid_server {
        openid_connect_mock::stop(mock_server_join_handle);
    }
}

async fn do_run_krill_ui_test(test_name: &str) {
    let dir = sub_dir(&PathBuf::from("work"));
    let test_dir = dir.to_string_lossy().to_string();

    env::set_var(KRILL_ENV_TEST_ANN, "1");
    env::set_var(KRILL_ENV_TEST, "1");

    let data_dir = PathBuf::from(test_dir);
    let mut config = Config::read_config(&format!("test-resources/ui/{}.conf", test_name)).unwrap();
    config.set_data_dir(data_dir);
    config.init_logging().unwrap();
    config.verify().unwrap();

    tokio::spawn(server::start(Some(config)));

    println!("Waiting for Krill server to start");
    assert!(server_ready().await);

    let test_name = test_name.to_string();

    task::spawn_blocking(move || {
        // NOTE: the directory mentioned here must be the same as the directory
        // mentioned in the tests/ui/cypress_plugins/index.js file in the
        // "integrationFolder" property otherwise Cypress mysteriously complains
        // that it cannot find the spec file.
        let cypress_spec_path = format!("tests/ui/cypress_specs/{}.js", test_name);

        Command::new("docker")
            .arg("run")
            .arg("--rm")
            .arg("--net=host")
            .arg("--ipc=host")
            .arg("-v")
            .arg(format!("{}:/e2e", env::current_dir().unwrap().display()))
            .arg("-w")
            .arg("/e2e")
            .arg("cypress/included:5.5.0")
            .arg("--browser")
            .arg("chrome")
            .arg("--spec")
            .arg(cypress_spec_path)
            .status()
            .expect("Failed to run Cypress Docker UI test suite");
    }).await.unwrap();
}