//! Protected retrieval adapters. Results are bounded text, never executable
//! content or client resource URLs. Credentials exist only in backend headers.
pub async fn call(
    client: &reqwest::Client,
    name: &str,
    args: &serde_json::Value,
) -> serde_json::Value {
    let result = run(client, name, args).await;
    match result {
        Ok(text) => serde_json::json!({"untrusted_reference":text}),
        Err(message) => serde_json::json!({"error":message}),
    }
}
async fn bounded(mut response: reqwest::Response) -> Result<Vec<u8>, &'static str> {
    if !response.status().is_success() {
        return Err("Retrieval service unavailable");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Retrieval interrupted")?
    {
        if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
            return Err("Retrieval byte limit reached");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
async fn run(
    client: &reqwest::Client,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, &'static str> {
    match name {
        "search_web" => {
            let key = std::env::var("EXA_API_KEY").map_err(|_| "Web search is not configured")?;
            let query = args["query"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or("Invalid search query")?;
            let response=client.post("https://api.exa.ai/search").header("x-api-key",key).json(&serde_json::json!({"query":query,"numResults":4,"contents":{"text":{"maxCharacters":1800}}})).send().await.map_err(|_|"Web search unavailable")?;
            let value: serde_json::Value = serde_json::from_slice(&bounded(response).await?)
                .map_err(|_| "Invalid search response")?;
            let rows=value["results"].as_array().ok_or("Invalid search response")?.iter().take(4).map(|result|serde_json::json!({"title":result["title"],"url":result["url"],"text":result["text"].as_str().unwrap_or("").chars().take(1800).collect::<String>()})).collect::<Vec<_>>();
            Ok(serde_json::to_string(&rows)
                .unwrap()
                .chars()
                .take(10000)
                .collect())
        }
        "fetch_page" => {
            let key =
                std::env::var("JINA_API_KEY").map_err(|_| "Page retrieval is not configured")?;
            let text = args["url"]
                .as_str()
                .filter(|s| s.len() <= 2048)
                .ok_or("Invalid page URL")?;
            let url = reqwest::Url::parse(text).map_err(|_| "Invalid page URL")?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("Unsupported page URL");
            }
            let host = url.host_str().ok_or("Invalid page host")?;
            if host == "localhost" || !host.contains('.') || host.ends_with(".local") {
                return Err("Only public pages can be retrieved");
            }
            if let Ok(address) = host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
                match address {
                    std::net::IpAddr::V4(ip)
                        if ip.is_private()
                            || ip.is_loopback()
                            || ip.is_link_local()
                            || ip.is_unspecified() =>
                    {
                        return Err("Only public pages can be retrieved")
                    }
                    std::net::IpAddr::V6(ip)
                        if ip.is_loopback()
                            || ip.is_unspecified()
                            || ip.is_unique_local()
                            || ip.is_unicast_link_local() =>
                    {
                        return Err("Only public pages can be retrieved")
                    }
                    _ => (),
                }
            }
            let response = client
                .get(format!("https://r.jina.ai/{url}"))
                .bearer_auth(key)
                .header("x-return-format", "text")
                .send()
                .await
                .map_err(|_| "Page retrieval unavailable")?;
            Ok(String::from_utf8_lossy(&bounded(response).await?)
                .chars()
                .take(12000)
                .collect())
        }
        _ => Err("Unsupported retrieval tool"),
    }
}
