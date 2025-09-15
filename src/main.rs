//! BetterStack Uptime Calculator
//!
//! This tool calculates and reports uptime statistics for BetterStack monitors
//! over a specified date range. It uses asynchronous programming to efficiently
//! process multiple monitors concurrently.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::Parser;
use dotenv::dotenv;
use futures::stream::{self, StreamExt};
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::{info, warn};
use url::Url;

/// Command line arguments for the BetterStack uptime calculator
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Start date in MM/DD/YYYY or MM-DD-YYYY format (e.g., 01/01/2024)
    #[arg(short, long, value_parser = parse_naive_date, requires = "end_date")]
    start_date: Option<NaiveDate>,

    /// End date in MM/DD/YYYY or MM-DD-YYYY format (e.g., 12/31/2024)
    #[arg(short, long, value_parser = parse_naive_date, requires = "start_date")]
    end_date: Option<NaiveDate>,

    /// Only include monitors that are published on status pages
    #[arg(long)]
    status_page_only: bool,

    /// Search terms to filter monitors (any term can match, case-insensitive)
    /// Can be specified multiple times: --search "premium us 02" --search "api"
    #[arg(long)]
    search: Vec<String>,

    /// When all monitors have no downtime (100% uptime), skip the per-monitor table and show only the average summary
    #[arg(long)]
    skip_no_downtime: bool,
}

/// Generic paginated response wrapper
#[derive(Debug, Deserialize)]
struct Paginated<T> {
    data: Vec<T>,
    pagination: Option<Pagination>,
}

#[derive(Debug, Deserialize)]
struct Pagination {
    next: Option<String>,
}

/// BetterStack monitor model
#[derive(Debug, Deserialize, Clone)]
struct Monitor {
    id: String,
    attributes: MonitorAttributes,
}

#[derive(Debug, Deserialize, Clone)]
struct MonitorAttributes {
    pronounceable_name: Option<String>,
    url: Option<String>,
}

/// SLA response types
#[derive(Debug, Deserialize)]
struct SlaResponse {
    data: SlaData,
}

#[derive(Debug, Deserialize)]
struct SlaData {
    attributes: SlaAttributes,
}

#[derive(Debug, Deserialize)]
struct SlaAttributes {
    availability: Option<f64>,
    total_downtime: Option<u64>,
}

/// Status page response types
#[derive(Debug, Deserialize)]
struct StatusPagesResponse {
    data: Vec<StatusPage>,
}

#[derive(Debug, Deserialize)]
struct StatusPage {
    id: String,
}

#[derive(Debug, Deserialize)]
struct ResourcesResponse {
    data: Vec<Resource>,
}

#[derive(Debug, Deserialize)]
struct Resource {
    attributes: ResourceAttributes,
}

#[derive(Debug, Deserialize)]
struct ResourceAttributes {
    resource_type: Option<String>,
    resource_id: JsonValue,
}

/// Incident data from BetterStack API
#[derive(Debug, Deserialize, Clone)]
struct Incident {
    #[allow(dead_code)]
    id: String,
    attributes: IncidentAttributes,
}

#[derive(Debug, Deserialize, Clone)]
struct IncidentAttributes {
    started_at: Option<String>,
    resolved_at: Option<String>,
    #[allow(dead_code)]
    acknowledger_name: Option<String>,
    #[allow(dead_code)]
    cause: Option<String>,
}

/// A downtime period extracted from incidents
#[derive(Debug, Clone, Serialize)]
struct DowntimePeriod {
    started_at: String,
    resolved_at: Option<String>,
    duration_mins: Option<i64>,
}

/// Result of uptime calculation for a monitor
#[derive(Debug, Clone, Serialize)]
struct UptimeCalc {
    id: String,
    name: String,
    percentage: f64,
    downtime: u64,
    downtime_mins: i64,
    uptime: u64,
    max_uptime: u64,
    downtime_periods: Vec<DowntimePeriod>,
}

/// BetterStack API client for making authenticated requests
#[derive(Clone)]
struct BetterStackApi {
    /// Base URL for the BetterStack API
    api_uri: String,
    /// HTTP client with authentication headers
    client: Client,
}

impl BetterStackApi {
    /// Creates a new BetterStack API client with the provided API token and base URL
    ///
    /// # Arguments
    ///
    /// * `api_key` - The BetterStack API token for authentication
    /// * `api_uri` - The base URL for the BetterStack API (e.g., <https://uptime.betterstack.com/api/v2>)
    ///
    /// # Returns
    ///
    /// A new `BetterStackApi` instance configured with authentication headers
    fn new(api_key: &str, api_uri: &str) -> Result<Self, Box<dyn Error>> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "Authorization",
            header::HeaderValue::from_str(&format!("Bearer {api_key}"))?,
        );

        let client = Client::builder().default_headers(headers).build()?;

        Ok(BetterStackApi {
            api_uri: api_uri.to_string(),
            client,
        })
    }

    /// Retrieves all monitors from the BetterStack account
    ///
    /// Paginates through the `/monitors` endpoint until no `next` page is present.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Monitor>)` on success
    /// * `Err` if the HTTP request fails or the response cannot be parsed
    async fn get_monitors(&self) -> Result<Vec<Monitor>, Box<dyn Error>> {
        let mut all: Vec<Monitor> = Vec::new();
        let mut page: u32 = 1;

        loop {
            let mut url = Url::parse(&self.api_uri)?;
            url.path_segments_mut()
                .map_err(|_| "cannot be base")?
                .extend(["monitors"]);
            url.query_pairs_mut().append_pair("page", &page.to_string());

            let resp = self.client.get(url).send().await?;
            if !resp.status().is_success() {
                return Err(format!("failed to fetch monitors page {page}").into());
            }
            let parsed: Paginated<Monitor> = resp.json().await?;
            all.extend(parsed.data);

            match parsed.pagination.and_then(|p| p.next) {
                Some(_) => {
                    page += 1;
                }
                None => break,
            }
        }

        Ok(all)
    }

    /// Retrieves SLA (Service Level Agreement) data for a specific monitor
    ///
    /// # Arguments
    ///
    /// * `monitor_id` - The ID of the monitor to query
    /// * `from` - Start of the date range (UTC, inclusive)
    /// * `to` - End of the date range (UTC, inclusive)
    ///
    /// # Returns
    ///
    /// * `Ok(SlaResponse)` with availability and downtime attributes
    /// * `Err` if the HTTP request fails or JSON parsing fails
    async fn get_monitor_sla(
        &self,
        monitor_id: &str,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Result<SlaResponse, Box<dyn Error>> {
        let mut url = Url::parse(&self.api_uri)?;
        url.path_segments_mut()
            .map_err(|_| "cannot be base")?
            .extend(["monitors", monitor_id, "sla"]);
        url.query_pairs_mut()
            .append_pair("from", &from.format("%Y-%m-%d").to_string())
            .append_pair("to", &to.format("%Y-%m-%d").to_string());

        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err("failed to fetch monitor SLA".into());
        }
        Ok(resp.json::<SlaResponse>().await?)
    }

    /// Retrieves all status pages from the BetterStack account
    ///
    /// Uses the legacy `betteruptime.com` host for status page APIs by
    /// rewriting the base host from `uptime.betterstack.com`.
    ///
    /// # Returns
    ///
    /// * `Ok(StatusPagesResponse)` on success
    /// * `Err` if the request fails or the response cannot be parsed
    async fn get_status_pages(&self) -> Result<StatusPagesResponse, Box<dyn Error>> {
        let mut base = Url::parse(&self.api_uri)?;
        base.set_host(Some(
            &base
                .host_str()
                .unwrap_or("")
                .replace("uptime.betterstack.com", "betteruptime.com"),
        ))?;
        let mut url = base;
        url.path_segments_mut()
            .map_err(|_| "cannot be base")?
            .extend(["status-pages"]);

        let resp = self.client.get(url).send().await?;
        if !resp.status().is_success() {
            return Err("failed to fetch status pages".into());
        }
        Ok(resp.json::<StatusPagesResponse>().await?)
    }

    /// Retrieves all resources (monitors) for a specific status page
    ///
    /// Paginates through all pages and aggregates results into a single list.
    ///
    /// # Arguments
    ///
    /// * `status_page_id` - The status page identifier
    ///
    /// # Returns
    ///
    /// * `Ok(ResourcesResponse)` whose `data` contains all resources
    /// * `Err` if any page fails to load or parse
    async fn get_status_page_resources(
        &self,
        status_page_id: &str,
    ) -> Result<ResourcesResponse, Box<dyn Error>> {
        let mut url = Url::parse(&self.api_uri)?;
        url.path_segments_mut()
            .map_err(|_| "cannot be base")?
            .extend(["status-pages", status_page_id, "resources"]);
        url.query_pairs_mut().append_pair("page", "1");

        // paginate
        let mut all: Vec<Resource> = Vec::new();
        let mut page: u32 = 1;
        loop {
            let mut page_url = url.clone();
            page_url
                .query_pairs_mut()
                .clear()
                .append_pair("page", &page.to_string());

            let resp = self.client.get(page_url).send().await?;
            if !resp.status().is_success() {
                return Err("failed to fetch status page resources".into());
            }
            let parsed: Paginated<Resource> = resp.json().await?;
            all.extend(parsed.data);
            match parsed.pagination.and_then(|p| p.next) {
                Some(_) => page += 1,
                None => break,
            }
        }

        Ok(ResourcesResponse { data: all })
    }

    /// Retrieves all monitor IDs that are published on status pages
    ///
    /// Combines status page listing and their resources, filtering resources
    /// whose `resource_type` is `Monitor` and collecting their IDs.
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<String>)` of deduplicated monitor IDs
    /// * `Err` if fetching pages or resources fails
    async fn get_status_page_monitors(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let pages = self.get_status_pages().await?;
        let mut ids: HashSet<String> = HashSet::new();

        for page in pages.data {
            let resources = self.get_status_page_resources(&page.id).await?;
            for resource in resources.data {
                if matches!(
                    resource.attributes.resource_type.as_deref(),
                    Some("Monitor")
                ) {
                    let id_str = match &resource.attributes.resource_id {
                        JsonValue::String(s) => s.clone(),
                        JsonValue::Number(n) => n.to_string(),
                        _ => continue,
                    };
                    ids.insert(id_str);
                }
            }
        }

        Ok(ids.into_iter().collect())
    }

    /// Retrieves incidents/downtime events for a specific monitor
    ///
    /// Tries multiple possible API endpoints to find downtime events:
    /// 1. /monitors/{id}/incidents
    /// 2. /incidents?monitor_id={id}
    /// 3. /monitors/{id}/events
    /// 4. /heartbeats?monitor_id={id} (failed heartbeats indicate downtime)
    ///
    /// # Arguments
    ///
    /// * `monitor_id` - The ID of the monitor to query
    /// * `from` - Start of the date range (UTC, inclusive)
    /// * `to` - End of the date range (UTC, inclusive)
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<Incident>)` with incident data
    /// * `Err` if all API endpoints fail
    async fn get_monitor_incidents(
        &self,
        monitor_id: &str,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Result<Vec<Incident>, Box<dyn Error>> {
        // Try multiple possible endpoints
        let endpoints = vec![
            format!("/monitors/{}/incidents", monitor_id),
            format!("/incidents?monitor_id={}", monitor_id),
            format!("/monitors/{}/events", monitor_id),
            format!("/monitors/{}/heartbeats", monitor_id),
        ];

        for endpoint_path in endpoints {
            match self.try_fetch_incidents(&endpoint_path, from, to).await {
                Ok(incidents) => {
                    if !incidents.is_empty() {
                        info!(
                            monitor_id = monitor_id,
                            endpoint = endpoint_path,
                            count = incidents.len(),
                            "Found incidents using endpoint"
                        );
                        return Ok(incidents);
                    }
                }
                Err(e) => {
                    // Only log warnings for first endpoint, others as debug
                    if endpoint_path.contains("/incidents") {
                        warn!(
                            monitor_id = monitor_id,
                            endpoint = endpoint_path,
                            error = %e,
                            "Primary incidents endpoint failed"
                        );
                    }
                }
            }
        }

        // If all endpoints fail, return empty list
        Ok(Vec::new())
    }

    /// Helper method to try fetching incidents from a specific endpoint
    async fn try_fetch_incidents(
        &self,
        endpoint_path: &str,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Result<Vec<Incident>, Box<dyn Error>> {
        let mut all: Vec<Incident> = Vec::new();
        let mut page: u32 = 1;

        loop {
            let mut url = Url::parse(&self.api_uri)?;
            
            // Handle different endpoint formats
            if endpoint_path.contains('?') {
                // Query parameter format like /incidents?monitor_id=123
                let parts: Vec<&str> = endpoint_path.splitn(2, '?').collect();
                url.path_segments_mut()
                    .map_err(|_| "cannot be base")?
                    .extend(parts[0].trim_start_matches('/').split('/').filter(|s| !s.is_empty()));
                url.set_query(Some(parts[1]));
            } else {
                // Path format like /monitors/123/incidents
                url.path_segments_mut()
                    .map_err(|_| "cannot be base")?
                    .extend(endpoint_path.trim_start_matches('/').split('/').filter(|s| !s.is_empty()));
            }
            
            url.query_pairs_mut()
                .append_pair("page", &page.to_string())
                .append_pair("from", &from.format("%Y-%m-%d").to_string())
                .append_pair("to", &to.format("%Y-%m-%d").to_string());

            let resp = self.client.get(url.clone()).send().await?;
            if !resp.status().is_success() {
                return Err(format!(
                    "HTTP {} from endpoint {}",
                    resp.status().as_u16(),
                    endpoint_path
                )
                .into());
            }
            
            let parsed: Paginated<Incident> = resp.json().await?;
            all.extend(parsed.data);

            match parsed.pagination.and_then(|p| p.next) {
                Some(_) => {
                    page += 1;
                }
                None => break,
            }
        }

        Ok(all)
    }

    /// Calculates uptime statistics for a specific monitor
    ///
    /// Fetches SLA data for the given date range and converts the reported
    /// availability and downtime into derived values like uptime seconds and
    /// downtime minutes.
    ///
    /// # Arguments
    ///
    /// * `monitor_id` - The monitor ID
    /// * `monitor_name` - Human-friendly monitor name used in output
    /// * `from` - Start of the date range (UTC)
    /// * `to` - End of the date range (UTC)
    ///
    /// # Returns
    ///
    /// * `Ok(UptimeCalc)` containing computed metrics for the monitor
    /// * `Err` if fetching SLA data fails or parsing fails
    async fn calculate_uptime(
        &self,
        monitor_id: &str,
        monitor_name: &str,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Result<UptimeCalc, Box<dyn Error>> {
        let sla = self.get_monitor_sla(monitor_id, from, to).await?;
        let attrs = sla.data.attributes;
        let availability = attrs.availability.unwrap_or(0.0);
        let total_downtime_secs = attrs.total_downtime.unwrap_or(0);
        let downtime_mins = (total_downtime_secs / 60) as i64;

        let total_seconds = (to.timestamp() - from.timestamp()).max(0) as u64;
        let uptime_seconds = (total_seconds as f64 * (availability / 100.0)) as u64;

        // Fetch incidents only if there is downtime
        let incidents = if total_downtime_secs > 0 {
            self.get_monitor_incidents(monitor_id, from, to)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let mut downtime_periods = Vec::new();

        for incident in incidents {
            if let Some(started_at) = incident.attributes.started_at {
                let duration_mins = if let Some(resolved_at) = &incident.attributes.resolved_at {
                    // Parse the dates and calculate duration
                    match (
                        DateTime::parse_from_rfc3339(&started_at),
                        DateTime::parse_from_rfc3339(resolved_at),
                    ) {
                        (Ok(start), Ok(end)) => {
                            let duration = end.signed_duration_since(start);
                            Some(duration.num_minutes())
                        }
                        _ => None,
                    }
                } else {
                    None
                };

                downtime_periods.push(DowntimePeriod {
                    started_at,
                    resolved_at: incident.attributes.resolved_at,
                    duration_mins,
                });
            }
        }

        Ok(UptimeCalc {
            id: monitor_id.to_string(),
            name: monitor_name.to_string(),
            percentage: availability,
            downtime: total_downtime_secs,
            downtime_mins,
            uptime: uptime_seconds,
            max_uptime: total_seconds,
            downtime_periods,
        })
    }
}

/// Parses a date string into a NaiveDate
///
/// Supports two formats:
/// * MM/DD/YYYY (e.g., 01/31/2024)
/// * MM-DD-YYYY (e.g., 01-31-2024)
///
/// # Returns
///
/// * `Ok(NaiveDate)` on success
/// * `Err(String)` with the parsing error message if both formats fail
fn parse_naive_date(date_str: &str) -> Result<NaiveDate, String> {
    if let Ok(d) = NaiveDate::parse_from_str(date_str, "%m/%d/%Y") {
        return Ok(d);
    }
    NaiveDate::parse_from_str(date_str, "%m-%d-%Y").map_err(|e| e.to_string())
}

/// Main entry point for the BetterStack uptime calculator
///
/// This function:
/// 1. Parses command line arguments for date range
/// 2. Loads API credentials from environment variables
/// 3. Fetches all monitors from BetterStack
/// 4. Calculates uptime statistics for each monitor concurrently
/// 5. Outputs results sorted by monitor name
///
/// # Returns
///
/// * `Ok(())` on success, or an `Err` describing the failure
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv().ok(); // Load .env file if it exists
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();

    let (start_date, end_date) = if let (Some(s), Some(e)) = (args.start_date, args.end_date) {
        let start = Utc.from_utc_datetime(&s.and_hms_opt(0, 0, 0).ok_or("invalid start time")?);
        let end = Utc.from_utc_datetime(&e.and_hms_opt(0, 0, 0).ok_or("invalid end time")?);
        (start, end)
    } else {
        let end_date = Utc::now();
        let start_date = end_date - chrono::Duration::days(365);
        (start_date, end_date)
    };

    info!(
        start = %start_date.format("%Y-%m-%d"),
        end = %end_date.format("%Y-%m-%d"),
        "Calculating uptime"
    );

    let api_token = env::var("BETTERSTACK_API_TOKEN")?;
    let api_url = env::var("BETTERSTACK_API_URL")
        .unwrap_or_else(|_| "https://uptime.betterstack.com/api/v2".to_string());
    let betterstack_api = BetterStackApi::new(&api_token, &api_url)?;

    let monitors = betterstack_api.get_monitors().await?;

    // Filter monitors based on status page flag
    let monitors_to_process: Vec<Monitor> = if args.status_page_only {
        info!("Fetching monitors from status pages...");
        let status_page_monitor_ids = betterstack_api.get_status_page_monitors().await?;
        let id_set: HashSet<String> = status_page_monitor_ids.into_iter().collect();

        let filtered: Vec<Monitor> = monitors
            .into_iter()
            .filter(|m| id_set.contains(&m.id))
            .collect();

        info!(
            filtered = filtered.len(),
            total = id_set.len(),
            "Found monitors on status pages"
        );

        filtered
    } else {
        info!(count = monitors.len(), "Processing monitors");
        monitors
    };

    let sem = Arc::new(Semaphore::new(10));
    let start_date_c = start_date;
    let end_date_c = end_date;

    let uptime_results = stream::iter(monitors_to_process.into_iter())
        .map(|m| {
            let api = betterstack_api.clone();
            let sem = Arc::clone(&sem);
            let start = start_date_c;
            let end = end_date_c;
            async move {
                let _permit = sem
                    .acquire_owned()
                    .await
                    .map_err(|e| format!("semaphore: {e}"))?;
                let name = m
                    .attributes
                    .pronounceable_name
                    .or(m.attributes.url)
                    .unwrap_or_else(|| "Unknown".to_string());
                api.calculate_uptime(&m.id, &name, &start, &end).await
            }
        })
        .buffer_unordered(10)
        .collect::<Vec<_>>()
        .await;

    let mut uptime_calculations: Vec<UptimeCalc> = uptime_results
        .into_iter()
        .filter_map(|r| r.map_err(|e| warn!(error = %e, "calculation failed")).ok())
        .collect();

    // Apply search filter if provided
    if !args.search.is_empty() {
        let terms: Vec<String> = args.search.iter().map(|s| s.to_lowercase()).collect();
        uptime_calculations.retain(|u| {
            let name_lower = u.name.to_lowercase();
            terms.iter().any(|t| name_lower.contains(t))
        });

        if uptime_calculations.is_empty() {
            println!(
                "\nNo monitors found matching any of the search terms: {}",
                args.search.join(", ")
            );
            return Ok(());
        }

        println!(
            "\nFound {} monitors matching any of the search terms: {}",
            uptime_calculations.len(),
            args.search.join(", ")
        );
    }

    // Sort monitors: 100% uptime first (alphabetically), then <100% from best to worst
    uptime_calculations.sort_by(|a, b| {
        let ap = a.percentage;
        let bp = b.percentage;
        match (ap == 100.0, bp == 100.0) {
            (true, true) => a.name.cmp(&b.name),
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            (false, false) => bp.partial_cmp(&ap).unwrap_or(std::cmp::Ordering::Equal),
        }
    });

    let no_downtime = uptime_calculations.iter().all(|u| u.percentage == 100.0);
    if args.skip_no_downtime && no_downtime {
        let total_uptime: f64 = uptime_calculations.iter().map(|u| u.percentage).sum();
        let average_uptime = if !uptime_calculations.is_empty() {
            total_uptime / uptime_calculations.len() as f64
        } else {
            0.0
        };
        if average_uptime == 100.0 {
            println!("\nAverage Uptime: 100%");
        } else {
            println!("\nAverage Uptime: {average_uptime:.4}%");
        }
        println!("Total Monitors: {}", uptime_calculations.len());
        return Ok(());
    }

    // Check if we have any monitors with downtime to determine table format
        let has_downtime = uptime_calculations.iter().any(|u| u.percentage < 100.0);

    let mut max_name_len = "Monitor Name".len();
    for u in &uptime_calculations {
        let name_len = u.name.len();
        if name_len > max_name_len {
            max_name_len = name_len;
        }
    }

    // Table header now always has two columns: Monitor Name | Status/Details
    if has_downtime {
        println!("\n{}", "=".repeat(max_name_len + 28));
        println!(
            "{:<width$} | {:<20}",
            "Monitor Name",
            "Status/Details",
            width = max_name_len
        );
        println!("{}", "=".repeat(max_name_len + 28));
    } else {
        // All monitors are 100% uptime; still show status column with explicit 100%
        println!("\n{}", "=".repeat(max_name_len + 28));
        println!(
            "{:<width$} | {:<20}",
            "Monitor Name",
            "Status",
            width = max_name_len
        );
        println!("{}", "=".repeat(max_name_len + 28));
    }

    let total_uptime: f64 = uptime_calculations.iter().map(|u| u.percentage).sum();
    let average_uptime = if !uptime_calculations.is_empty() {
        total_uptime / uptime_calculations.len() as f64
    } else {
        0.0
    };

    let mut printed_separator = false;
    for u in &uptime_calculations {
        let percentage = u.percentage;
        let name = &u.name;
        let downtime_mins = u.downtime_mins;

            if percentage < 100.0 && !printed_separator {
                let separator_len = if has_downtime { max_name_len + 28 } else { max_name_len + 8 };
                println!("{}", "-".repeat(separator_len));
                println!("Monitors with downtime (best to worst):");
                println!("{}", "-".repeat(separator_len));
                printed_separator = true;
            }

        if percentage == 100.0 {
            // Show explicit 100% status
            println!("{:<width$} | {:<20}", name, "100%", width = max_name_len);
        } else {
            // Simplify: if we have incident periods, we will list them beneath; no need for placeholder text
            let downtime_display = if u.downtime_periods.is_empty() {
                if downtime_mins > 0 {
                    "Details unavailable".to_string()
                } else {
                    "None".to_string()
                }
            } else {
                // Show uptime percentage instead of downtime minutes
                format!("{:.4}%", percentage)
            };

            #[allow(clippy::uninlined_format_args)]
            {
                println!(
                    "{:<width$} | {:<20}",
                    name,
                    downtime_display,
                    width = max_name_len
                );
            }
        }

        // Show detailed downtime periods for monitors with downtime
        if percentage < 100.0 {
            if !u.downtime_periods.is_empty() {
                // We have actual incident data with timestamps
                for period in u.downtime_periods.iter() {
                    let started_utc = DateTime::parse_from_rfc3339(&period.started_at)
                        .map(|dt| dt.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S UTC").to_string())
                        .unwrap_or_else(|_| period.started_at.clone());
                    
                    let resolved_utc = match &period.resolved_at {
                        Some(resolved_str) => {
                            DateTime::parse_from_rfc3339(resolved_str)
                                .map(|dt| dt.with_timezone(&Utc).format("%Y-%m-%d %H:%M:%S UTC").to_string())
                                .unwrap_or_else(|_| resolved_str.clone())
                        }
                        None => "Ongoing".to_string(),
                    };
                    
                    let duration_str = match period.duration_mins {
                        Some(mins) => format!(" ({}m)", mins),
                        None => "".to_string(),
                    };
                    
                    println!(
                        "{:<width$} | {} =>",
                        "",
                        started_utc,
                        width = max_name_len
                    );
                    println!(
                        "{:<width$} | {}{}",
                        "",
                        resolved_utc,
                        duration_str,
                        width = max_name_len
                    );
                }
            } else if downtime_mins > 0 {
                // We have downtime but no detailed incident data
                println!(
                    "{:<width$} | (Time details unavailable)",
                    "",
                    width = max_name_len
                );
            }
        }
    }
    let final_separator_len = if has_downtime { max_name_len + 28 } else { max_name_len + 8 };
    println!("{}", "=".repeat(final_separator_len));

    if average_uptime == 100.0 {
        println!("\nAverage Uptime: 100%");
    } else {
        println!("\nAverage Uptime: {average_uptime:.4}%");
    }
    println!("Total Monitors: {}", uptime_calculations.len());

    Ok(())
}
