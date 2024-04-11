use super::app_config::AppConfig;
use super::app_config::ServiceConfig;
use super::app_config::StaticConfig;
use crate::configuration_service::logger;
use crate::constants;
use crate::constants::common_constants::ENV_ACCESS_LOG;
use crate::constants::common_constants::ENV_ADMIN_PORT;
use crate::constants::common_constants::ENV_CONFIG_FILE_PATH;
use crate::constants::common_constants::ENV_DATABASE_URL;
use crate::constants::common_constants::TIMER_WAIT_SECONDS;
use crate::control_plane::rest_api::start_control_plane;
use crate::proxy::http1::http_proxy::HttpProxy;
use crate::proxy::http2::grpc_proxy::GrpcProxy;
use crate::proxy::tcp::tcp_proxy::TcpProxy;
use crate::vojo::api_service_manager::ApiServiceManager;
use crate::vojo::app_config::{ApiService, ServiceType};
use crate::vojo::app_error::AppError;
use dashmap::DashMap;
use futures::FutureExt;
use lazy_static::lazy_static;
use log::Level;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::Mutex;
use tokio::sync::RwLock;
use tokio::time::sleep;
use uuid::Uuid;

#[derive(Clone)]
pub struct Handler {
    pub shared_app_config: Arc<Mutex<AppConfig>>,
}
impl Handler {
    pub async fn run(&mut self, admin_port: i32) -> Result<(), AppError> {
        let static_config = init_static_config().await;
        let map = init_app_service_config(self, static_config.clone()).await?;
        let mut lock = self.shared_app_config.lock().await;
        lock.api_service_config = map;
        lock.static_config = static_config;
        drop(lock);
        start_control_plane(self.clone(), admin_port).await;
        Ok(())
    }
    pub async fn start_proxy(
        &mut self,
        port: i32,
        channel: mpsc::Receiver<()>,
        server_type: ServiceType,
        mapping_key: String,
    ) -> Result<(), AppError> {
        if server_type == ServiceType::Http {
            let mut http_proxy = HttpProxy {
                port,
                channel,
                mapping_key: mapping_key.clone(),
            };
            http_proxy.start_http_server().await
        } else if server_type == ServiceType::Https {
            let key_clone = mapping_key.clone();

            let pem_str = String::from("");
            let key_str = String::from("");
            let mut http_proxy = HttpProxy {
                port,
                channel,
                mapping_key: mapping_key.clone(),
            };
            http_proxy.start_https_server(pem_str, key_str).await
        } else if server_type == ServiceType::Tcp {
            let mut tcp_proxy = TcpProxy {
                port,
                mapping_key,
                channel,
            };
            tcp_proxy.start_proxy().await
        } else if server_type == ServiceType::Http2 {
            let mut grpc_proxy = GrpcProxy {
                port,
                mapping_key,
                channel,
            };
            grpc_proxy.start_proxy().await
        } else {
            let key_clone = mapping_key.clone();
            // let service_config = GLOBAL_CONFIG_MAPPING
            //     .get(&key_clone)
            //     .unwrap()
            //     .service_config
            //     .clone();
            let pem_str = String::from("");
            let key_str = String::from("");
            let mut grpc_proxy = GrpcProxy {
                port,
                mapping_key,
                channel,
            };
            grpc_proxy.start_tls_proxy(pem_str, key_str).await
        }
    }
}
async fn init_static_config() -> StaticConfig {
    let mut static_config = StaticConfig::default();
    let database_url_result = env::var(ENV_DATABASE_URL);
    let api_port = env::var(ENV_ADMIN_PORT).unwrap_or(String::from(
        constants::common_constants::DEFAULT_ADMIN_PORT,
    ));
    let access_log_result = env::var(ENV_ACCESS_LOG);
    let config_file_path_result = env::var(ENV_CONFIG_FILE_PATH);

    if let Ok(database_url) = database_url_result {
        static_config.database_url = Some(database_url);
    }
    static_config.admin_port = api_port.clone();

    logger::start_logger();

    if let Ok(access_log) = access_log_result {
        static_config.access_log = Some(access_log);
    }

    if let Ok(config_file_path) = config_file_path_result {
        static_config.config_file_path = Some(config_file_path);
    }
    static_config
}
async fn init_app_service_config(
    handler: &mut Handler,
    static_config: StaticConfig,
) -> Result<HashMap<String, ApiService>, AppError> {
    let config_file_path = static_config.config_file_path.clone();
    if config_file_path.is_none() {
        return Ok(HashMap::new());
    }
    let file_path = config_file_path.unwrap().clone();
    info!("the config file is in{}", file_path.clone());
    let file = std::fs::File::open(file_path).map_err(|e| AppError(e.to_string()))?;
    let mut scrape_config: Vec<ApiService> = match serde_yaml::from_reader(file) {
        Ok(apiservices) => apiservices,
        Err(err) => return Err(AppError(err.to_string())),
    };

    let mut res = HashMap::new();
    for item in scrape_config.iter_mut() {
        let uuid = Uuid::new_v4().to_string();

        let port = item.listen_port;
        let server_type = item.clone().service_config.server_type;
        let (sender, receiver) = mpsc::channel(1);
        item.sender = sender;
        res.insert(uuid.clone(), item.clone());
        let mut cloned_handler = handler.clone();
        tokio::spawn(async move {
            cloned_handler
                .start_proxy(port, receiver, server_type, uuid)
                .await;
        });
    }
    Ok(res)
}