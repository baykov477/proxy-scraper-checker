pub async fn is_docker() -> bool {
    #[cfg(target_os = "linux")]
    {
        static CACHE: tokio::sync::OnceCell<bool> =
            tokio::sync::OnceCell::const_new();

        *CACHE
            .get_or_init(async move || {
                tokio::fs::try_exists("/.dockerenv").await.unwrap_or(false)
            })
            .await
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

pub fn is_http_url(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(move |parsed_url| {
        let scheme = parsed_url.scheme();
        (scheme == "http" || scheme == "https")
            && parsed_url.host_str().is_some()
    })
}

pub fn pretty_error(e: &color_eyre::Report) -> String {
    e.chain().map(ToString::to_string).collect::<Vec<_>>().join(" \u{2192} ")
}
