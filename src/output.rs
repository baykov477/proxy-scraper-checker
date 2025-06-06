use std::{
    io, iter,
    net::{IpAddr, Ipv4Addr},
    sync::Arc,
};

use color_eyre::eyre::WrapErr as _;

use crate::{
    config::Config,
    geodb::get_geodb_path,
    proxy::{Proxy, ProxyType},
    storage::ProxyStorage,
    utils::is_docker,
};

fn sort_by_timeout(proxy: &Proxy) -> tokio::time::Duration {
    proxy.timeout.unwrap_or(tokio::time::Duration::MAX)
}

fn sort_naturally(proxy: &Proxy) -> (ProxyType, Vec<u8>, u16) {
    let host_key = proxy.host.parse::<Ipv4Addr>().map_or_else(
        move |_| iter::repeat_n(u8::MAX, 4).chain(proxy.host.bytes()).collect(),
        move |ip| ip.octets().to_vec(),
    );
    (proxy.protocol.clone(), host_key, proxy.port)
}

#[derive(serde::Serialize)]
struct ProxyJson<'a> {
    protocol: ProxyType,
    username: Option<String>,
    password: Option<String>,
    host: String,
    port: u16,
    timeout: Option<f64>,
    exit_ip: Option<String>,
    geolocation: Option<maxminddb::geoip2::City<'a>>,
}

#[expect(clippy::too_many_lines)]
pub async fn save_proxies(
    config: Arc<Config>,
    storage: ProxyStorage,
) -> color_eyre::Result<()> {
    if config.output_json {
        let maybe_mmdb = if config.enable_geolocation {
            let geodb_path =
                get_geodb_path().await.wrap_err("failed to get GeoDB path")?;
            Some(
                tokio::task::spawn_blocking(move || {
                    maxminddb::Reader::open_mmap(geodb_path)
                })
                .await
                .wrap_err("failed to spawn tokio blocking task")?
                .wrap_err("failed to open GeoDB")?,
            )
        } else {
            None
        };

        let mut sorted_proxies: Vec<_> = storage.iter().collect();
        sorted_proxies.sort_by_key(move |p| sort_by_timeout(p));

        let mut proxy_dicts = Vec::with_capacity(sorted_proxies.len());

        for proxy in sorted_proxies {
            let geolocation = if let Some(mmdb) = &maybe_mmdb {
                if let Some(exit_ip) = proxy.exit_ip.clone() {
                    let exit_ip_addr: IpAddr = exit_ip.parse().wrap_err(
                        "failed to parse proxy's exit ip as IpAddr",
                    )?;
                    mmdb.lookup::<maxminddb::geoip2::City>(exit_ip_addr)
                        .wrap_err_with(move || {
                            format!("failed to lookup {exit_ip_addr} in GeoDB")
                        })?
                } else {
                    None
                }
            } else {
                None
            };

            proxy_dicts.push(ProxyJson {
                protocol: proxy.protocol.clone(),
                username: proxy.username.clone(),
                password: proxy.password.clone(),
                host: proxy.host.clone(),
                port: proxy.port,
                timeout: proxy.timeout.map(move |d| {
                    (d.as_secs_f64() * 100.0).round() / 100.0_f64
                }),
                exit_ip: proxy.exit_ip.clone(),
                geolocation,
            });
        }

        for (path, pretty) in [
            (config.output_path.join("proxies.json"), false),
            (config.output_path.join("proxies_pretty.json"), true),
        ] {
            match tokio::fs::remove_file(&path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).wrap_err_with(|| {
                    format!("failed to remove file {}", path.display())
                }),
            }?;
            let json_data = if pretty {
                serde_json::to_vec_pretty(&proxy_dicts)
                    .wrap_err("failed to serialize proxies to pretty json")?
            } else {
                serde_json::to_vec(&proxy_dicts)
                    .wrap_err("failed to serialize proxies to json")?
            };
            tokio::fs::write(&path, json_data).await.wrap_err_with(
                move || {
                    format!("failed to write proxies to {}", path.display())
                },
            )?;
        }
    }

    if config.output_txt {
        let mut sorted_proxies: Vec<_> = storage.iter().collect();
        if config.sort_by_speed {
            sorted_proxies.sort_by_key(move |p| sort_by_timeout(p));
        } else {
            sorted_proxies.sort_by_key(move |p| sort_naturally(p));
        }
        let mut grouped_proxies = storage.get_grouped();

        for (anonymous_only, directory) in
            [(false, "proxies"), (true, "proxies_anonymous")]
        {
            let directory_path = config.output_path.join(directory);
            match tokio::fs::remove_dir_all(&directory_path).await {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e).wrap_err_with(|| {
                    format!(
                        "failed to remove directory {}",
                        directory_path.display()
                    )
                }),
            }?;
            tokio::fs::create_dir_all(&directory_path).await.wrap_err_with(
                || {
                    format!(
                        "failed to create directory: {}",
                        directory_path.display()
                    )
                },
            )?;

            let text =
                create_proxy_list_str(&sorted_proxies, anonymous_only, true);
            tokio::fs::write(directory_path.join("all.txt"), text)
                .await
                .wrap_err_with(|| {
                    format!(
                        "failed to write proxies to {}",
                        directory_path.join("all.txt").display()
                    )
                })?;

            for (proto, proxies) in &mut grouped_proxies {
                if config.sort_by_speed {
                    proxies.sort_by_key(move |p| sort_by_timeout(p));
                } else {
                    proxies.sort_by_key(move |p| sort_naturally(p));
                }
                let text =
                    create_proxy_list_str(proxies, anonymous_only, false);
                tokio::fs::write(
                    directory_path.join(format!("{proto}.txt")),
                    text,
                )
                .await
                .wrap_err_with(|| {
                    format!(
                        "failed to write proxies to {}",
                        directory_path.join(format!("{proto}.txt")).display()
                    )
                })?;
            }
        }
    }

    let path = config.output_path.canonicalize().wrap_err_with(move || {
        format!("failed to canonicalize {}", config.output_path.display())
    })?;
    if is_docker().await {
        log::info!(
            "Proxies have been saved to ./out ({} in container)",
            path.display()
        );
    } else {
        log::info!("Proxies have been saved to {}", path.display());
    }

    Ok(())
}

fn create_proxy_list_str(
    proxies: &Vec<&Proxy>,
    anonymous_only: bool,
    include_protocol: bool,
) -> String {
    proxies
        .iter()
        .filter(move |proxy| {
            !anonymous_only
                || proxy
                    .exit_ip
                    .as_ref()
                    .is_some_and(move |ip| *ip != proxy.host)
        })
        .map(move |proxy| proxy.as_str(include_protocol))
        .collect::<Vec<_>>()
        .join("\n")
}
