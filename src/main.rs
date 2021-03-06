mod hasher;
mod util;

use duino_miner::error::MinerError;

use crate::hasher::Sha1Hasher;
use crate::util::{generate_8hex, get_pool_info};

use serde::{Deserialize, Serialize};

use std::fs::File;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, SystemTime};

use rand::Rng;

use log::{error, info, warn};

use clap::{AppSettings, Clap, Subcommand};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub username: String,
    pub device_name: String,
    pub device_type: String,
    pub chip_id: String,
    pub firmware: String,
    pub target_rate: u32,
}

#[derive(Clap)]
#[clap(version = "0.1", author = "Black H. <encomblackhat@gmail.com>")]
#[clap(setting = AppSettings::ColoredHelp)]
struct Opts {
    #[clap(short, long, default_value = "config.yaml")]
    config_file: String,
    #[clap(subcommand)]
    sub_command: SubCommands,
}

#[derive(Subcommand)]
enum SubCommands {
    #[clap(version = "0.1", author = "Black H. <encomblackhat@gmail.com>")]
    Generate(Generate),
    Run(Run),
}

#[derive(Clap)]
struct Generate {
    #[clap(short, long, default_value = "my_username")]
    username: String,
    #[clap(long, default_value = "16")]
    device_count: u32,
    #[clap(long, default_value = "avr-")]
    device_name_prefix: String,
    #[clap(long, default_value = "AVR")]
    device_type: String,
    #[clap(long, default_value = "Official AVR Miner v2.6")]
    firmware: String,
    #[clap(long, default_value = "190")]
    target_rate: u32,
}

#[derive(Clap)]
struct Run {
    #[clap(short, long)]
    pool: Option<String>,
}

fn generate_config(file_path: String, gen: &Generate) -> Result<(), Box<dyn std::error::Error>> {
    let mut device_vec: Vec<Device> = Vec::new();

    for i in 0..gen.device_count {
        let device = Device {
            username: gen.username.clone(),
            device_name: format!("{}{}", gen.device_name_prefix, i + 1),
            device_type: gen.device_type.clone(),
            chip_id: format!("DUCOID{}", generate_8hex()),
            firmware: gen.firmware.clone(),
            target_rate: gen.target_rate,
        };

        device_vec.push(device);
    }

    let c = Config {
        devices: device_vec,
    };
    let c_serial = serde_yaml::to_string(&c)?;

    let mut f = File::create(file_path)?;
    f.write_all(c_serial.as_bytes())?;

    Ok(())
}

fn start_miner(device: Device, pool: String, hasher: Sha1Hasher) -> Result<(), MinerError> {
    let heatup_duration: u64 = rand::thread_rng().gen_range(10..10000);
    std::thread::sleep(Duration::from_millis(heatup_duration));

    let mut stream = TcpStream::connect(&pool).map_err(|_| MinerError::Connection)?;

    info!("{} connected to pool {}", device.device_name, pool);

    let mut cmd_in: [u8; 200] = [0; 200];
    let n = stream
        .read(&mut cmd_in)
        .map_err(|_| MinerError::RecvCommand)?;
    info!(
        "version: {}",
        std::str::from_utf8(&cmd_in[..n]).map_err(|_| MinerError::InvalidUTF8)?
    );

    let expected_interval = 1000000u128 / device.target_rate as u128;

    loop {
        let cmd_job = format!("JOB,{},{}\n", device.username, device.device_type);
        stream
            .write(cmd_job.as_bytes())
            .map_err(|_| MinerError::SendCommand)?;

        let n = stream
            .read(&mut cmd_in)
            .map_err(|_| MinerError::RecvCommand)?;
        let job = std::str::from_utf8(&cmd_in[..n])
            .map_err(|_| MinerError::InvalidUTF8)?
            .trim();

        let args: Vec<&str> = job.split(',').collect();
        if args.len() < 3 {
            return Err(MinerError::MalformedJob(job.to_string()));
        }

        let last_block_hash = args[0];
        let expected_hash = args[1];
        let diff = args[2]
            .parse::<u32>()
            .map_err(|_| MinerError::MalformedJob(job.to_string()))?
            * 100
            + 1;

        info!(
            "last: {}, expected: {}, diff: {}",
            last_block_hash, expected_hash, diff
        );

        let start = SystemTime::now();

        let duco_numeric_result = hasher
            .get_hash(last_block_hash, expected_hash, diff)
            .unwrap_or(0);

        let end = SystemTime::now();
        let duration = end.duration_since(start).unwrap().as_micros();
        let real_rate = duco_numeric_result as f64 / duration as f64 * 1000000f64;

        let expected_duration = expected_interval * duco_numeric_result as u128;

        if duration < expected_duration {
            let wait_duration = (expected_duration - duration) as u64;
            std::thread::sleep(Duration::from_micros(wait_duration));
            info!("waited {} micro sec", wait_duration);
        } else {
            warn!(
                "system too slow, lag {} micro sec",
                duration - expected_duration
            );
        }

        let end = SystemTime::now();
        let duration = end.duration_since(start).unwrap().as_micros();
        let emu_rate = duco_numeric_result as f64 / duration as f64 * 1000000f64;

        // let lag_duration: u64 = rand::thread_rng().gen_range(0..100);
        // tokio::time::sleep(Duration::from_millis(lag_duration)).await;

        let cmd_out = format!(
            "{},{:.2},{},{},{}\n",
            duco_numeric_result, emu_rate, device.firmware, device.device_name, device.chip_id
        );
        stream
            .write(cmd_out.as_bytes())
            .map_err(|_| MinerError::SendCommand)?;

        let n = stream
            .read(&mut cmd_in)
            .map_err(|_| MinerError::RecvCommand)?;
        let resp = std::str::from_utf8(&cmd_in[..n])
            .map_err(|_| MinerError::InvalidUTF8)?
            .trim();

        if resp == "GOOD" {
            info!(
                "result good, result: {}, rate: {:.2}, real: {:.2}",
                duco_numeric_result, emu_rate, real_rate
            );
        } else if resp == "BLOCK" {
            info!(
                "FOUND BLOCK!, result: {}, rate: {:.2}, real: {:.2}",
                duco_numeric_result, emu_rate, real_rate
            );
        } else {
            warn!(
                "resp: {}, result: {}, rate: {:.2}, real: {:.2}",
                resp, duco_numeric_result, emu_rate, real_rate
            );
        }
    }
}

fn start_miner_loop(device: Device, pool: Option<String>, hasher: Sha1Hasher) {
    info!("Spawning {}...", device.device_name);

    loop {
        let pool = if let Some(pool) = pool.clone() {
            pool
        } else {
            get_pool_info().unwrap_or(format!("{}:{}", "server.duinocoin.com", 2813))
        };

        match start_miner(device.clone(), pool, hasher.clone()) {
            Ok(_) => error!("exited without error"),
            Err(e) => error!("exited with error: {:?}", e),
        }
    }
}

fn start_miners(devices: Vec<Device>, pool: Option<String>, hasher: Sha1Hasher) {
    let mut handles = vec![];

    for device in devices {
        let hasher = hasher.clone();
        let pool = pool.clone();

        let handle = std::thread::spawn(move || {
            start_miner_loop(device, pool, hasher);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    let opts: Opts = Opts::parse();

    match opts.sub_command {
        SubCommands::Generate(gen) => {
            generate_config(opts.config_file, &gen)?;
        }
        SubCommands::Run(run) => {
            let c_serial = std::fs::read_to_string(opts.config_file)?;
            let c: Config = serde_yaml::from_str(c_serial.as_str())?;

            info!("running with {} miners", c.devices.len());

            let hasher = Sha1Hasher::new();
            start_miners(c.devices, run.pool, hasher);
        }
    }

    Ok(())
}
