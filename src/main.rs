use anyhow::Result;
use embedded_svc::http::client::Client;
use embedded_svc::http::client::Request;
use embedded_svc::{
    http::{Headers, SendHeaders, Status},
    storage::Storage,
    wifi::{self, Wifi},
};
use esp_idf_svc::http::client::EspHttpClient;
use esp_idf_svc::{
    http::client::{EspHttpClientConfiguration, FollowRedirectsPolicy},
    netif,
    nvs::EspDefaultNvs,
    nvs_storage::EspNvsStorage,
    sysloop,
    wifi::EspWifi,
};
use esp_idf_sys::{self as _, c_types::c_void, esp};
use log::*;
use semver::Version;
use serde::Deserialize;
use std::{ptr, sync::Arc};

use embedded_svc::http::client::Response;
use embedded_svc::io;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const UPDATE_URL: &str = "https://api.github.com/repos/anichno/esp32-ota-demo/releases";
const PRE_RELEASE: bool = false;

const WIFI_SSID_KEY: &str = "wifi-ssid";
const WIFI_PASS_KEY: &str = "wifi-pass";

#[derive(Deserialize, Debug)]
struct Release {
    tag_name: String,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize, Debug)]
struct Asset {
    url: String,
    name: String,
    content_type: String,
    size: usize,
}

fn ota_update_from_reader(reader: &mut impl embedded_svc::io::Read) -> Result<()> {
    let next_partition = unsafe { esp_idf_sys::esp_ota_get_next_update_partition(ptr::null()) };
    let mut ota_handle = esp_idf_sys::esp_ota_handle_t::default();
    esp!(unsafe { esp_idf_sys::esp_ota_begin(next_partition, 0, &mut ota_handle) })?;
    let mut buf = [0; 1024];
    let mut tot = 0;
    loop {
        let bytes_read = reader.do_read(&mut buf)?;
        if bytes_read == 0 {
            break;
        }
        tot += bytes_read;
        info!("read {} bytes so far", tot);

        esp!(unsafe {
            esp_idf_sys::esp_ota_write(ota_handle, buf.as_ptr() as *const c_void, bytes_read as u32)
        })?;
    }

    esp!(unsafe { esp_idf_sys::esp_ota_end(ota_handle) })?;
    esp!(unsafe { esp_idf_sys::esp_ota_set_boot_partition(next_partition) })?;
    unsafe {
        esp_idf_sys::esp_restart();
    }

    Ok(())
}

fn ota_update() -> Result<()> {
    info!("ota start");
    let config = EspHttpClientConfiguration {
        buffer_size: Some(8192),
        buffer_size_tx: Some(1024),
        follow_redirects_policy: FollowRedirectsPolicy::FollowNone,
    };
    let mut client = EspHttpClient::new(&config)?;

    let response = client.get(UPDATE_URL)?.submit()?;
    let mut reader = response.reader();
    let releases: Vec<Release> = serde_json::from_reader(io::StdIO(&mut reader))?;

    let cur_version = Version::parse(VERSION)?;
    for release in releases {
        let new_version = Version::parse(&release.tag_name)?;
        if new_version > cur_version && (PRE_RELEASE || !release.prerelease) {
            for asset in release.assets {
                if asset.content_type == "application/octet-stream" && asset.name.ends_with(".bin")
                {
                    info!("found new firmware with size: {}", asset.size);
                    // do update
                    let response = client
                        .get(&asset.url)?
                        .header("Accept", "application/octet-stream")
                        .submit()?;
                    if response.status() == 302 {
                        let location = String::from(response.header("Location").unwrap());
                        let response = client
                            .get(&location)?
                            .header("Accept", "application/octet-stream")
                            .submit()?;

                        let mut reader = response.reader();
                        ota_update_from_reader(&mut reader)?;
                    } else {
                        let mut reader = response.reader();
                        ota_update_from_reader(&mut reader)?;
                    }
                }
            }
        }
    }

    info!("No new firmware found");

    Ok(())
}

fn first_run_validate() -> Result<()> {
    unsafe {
        let cur_partition = esp_idf_sys::esp_ota_get_running_partition();
        let mut ota_state: esp_idf_sys::esp_ota_img_states_t = 0;
        if let Ok(()) = esp!(esp_idf_sys::esp_ota_get_state_partition(
            cur_partition,
            &mut ota_state
        )) {
            if ota_state == esp_idf_sys::esp_ota_img_states_t_ESP_OTA_IMG_PENDING_VERIFY {
                // Validate image
                esp!(esp_idf_sys::esp_ota_mark_app_valid_cancel_rollback())?;
            }
        }
    }

    Ok(())
}

fn main() -> Result<()> {
    // Temporary. Will disappear once ESP-IDF 4.4 is released, but for now it is necessary to call this function once,
    // or else some patches to the runtime implemented by esp-idf-sys might not link properly.
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    first_run_validate()?;
    let nvs = Arc::new(EspDefaultNvs::new()?);
    #[allow(unused_mut)]
    let mut storage = EspNvsStorage::new_default(nvs.clone(), "config", true)?;

    #[cfg(feature = "factory")]
    if !storage.contains("configured")? {
        storage.put(WIFI_SSID_KEY, &env!("WIFI_SSID"))?;
        storage.put(WIFI_PASS_KEY, &env!("WIFI_PASS"))?;
        storage.put("configured", &"true")?;
        info!("stored creds");
    }

    let ssid: String = storage.get(WIFI_SSID_KEY)?.unwrap();
    let password: String = storage.get(WIFI_PASS_KEY)?.unwrap();

    println!("wifi creds: {} : {}", ssid, password);
    println!("running version: {}", VERSION);
    println!("hello from app! OTA in 5 seconds");
    std::thread::sleep(std::time::Duration::from_secs(5));

    let mut wifi = EspWifi::new(
        Arc::new(netif::EspNetifStack::new()?),
        Arc::new(sysloop::EspSysLoopStack::new()?),
        nvs,
    )?;
    wifi.set_configuration(&wifi::Configuration::Client(wifi::ClientConfiguration {
        ssid,
        password,
        ..Default::default()
    }))?;

    ota_update()?;

    Ok(())
}
