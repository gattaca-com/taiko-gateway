use driver::TaikoDriver;
use pc_common::{
    config::{StaticConfig, TaikoChainConfig},
    driver::{DriverRequest, DriverResponse},
    types::DuplexChannel,
};
use tracing::info;

mod driver;
mod fetcher;

pub fn start_driver(
    config: &StaticConfig,
    chain_config: TaikoChainConfig,
    to_sequencer: DuplexChannel<DriverResponse, DriverRequest>,
) {
    let taiko_config = config.into();
    let l1_rpc_url = config.l1.rpc_url.clone();
    let l1_ws_url = config.l1.ws_url.clone();

    info!("starting driver");
    let driver = TaikoDriver::new(l1_rpc_url, l1_ws_url, taiko_config, chain_config, to_sequencer);
    std::thread::Builder::new()
        .name("driver".to_string())
        .spawn(move || {
            driver.run();
        })
        .expect("failed to start driver thread");
}
