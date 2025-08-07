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
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use tokio::time::{sleep, Duration};

/// Command line arguments for the BetterStack uptime calculator
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Start date in MM/DD/YYYY format (e.g., 01/01/2024)
    #[arg(short, long)]
    start_date: Option<String>,

    /// End date in MM/DD/YYYY format (e.g., 12/31/2024)
    #[arg(short, long)]
    end_date: Option<String>,

    /// Only include monitors that are published on status pages
    #[arg(long, action = clap::ArgAction::SetTrue)]
    status_page_only: bool,

    /// Search terms to filter monitors (any term can match, case-insensitive)
    /// Can be specified multiple times: --search "premium us 02" --search "api"
    #[arg(long, action = clap::ArgAction::Append)]
    search: Vec<String>,
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
    fn new(api_key: &str, api_uri: &str) -> Self {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            "Authorization",
            header::HeaderValue::from_str(&format!("Bearer {api_key}")).unwrap(),
        );

        let client = Client::builder().default_headers(headers).build().unwrap();

        BetterStackApi {
            api_uri: api_uri.to_string(),
            client,
        }
    }

    /// Retrieves all monitors from the BetterStack account
    ///
    /// # Returns
    ///
    /// * `Ok(String)` - JSON string containing all monitors data
    /// * `Err` - If the API request fails
    async fn get_monitors(&self) -> Result<String, Box<dyn Error>> {
        let mut all_monitors = Vec::new();
        let mut page = 1;

        loop {
            let response = self
                .client
                .get(format!("{}/monitors?page={}", self.api_uri, page))
                .send()
                .await?;

            let response_text = response.text().await?;
            let response_json: Value = serde_json::from_str(&response_text)?;

            // Extract monitors from this page
            if let Some(data) = response_json["data"].as_array() {
                all_monitors.extend(data.clone());
            }

            // Check if there's a next page
            if response_json["pagination"]["next"].is_null() {
                break;
            }

            page += 1;
        }

        // Construct the final response with all monitors
        let final_response = serde_json::json!({
            "data": all_monitors
        });

        Ok(final_response.to_string())
    }

    /// Retrieves SLA (Service Level Agreement) data for a specific monitor
    ///
    /// # Arguments
    ///
    /// * `monitor_id` - The ID of the monitor to get SLA data for
    /// * `from` - Start date for the SLA calculation
    /// * `to` - End date for the SLA calculation
    ///
    /// # Returns
    ///
    /// * `Ok(String)` - JSON string containing the monitor's SLA data
    /// * `Err` - If the API request fails
    async fn get_monitor_sla(
        &self,
        monitor_id: &str,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Result<String, Box<dyn Error>> {
        let url = format!(
            "{}/monitors/{}/sla?from={}&to={}",
            self.api_uri,
            monitor_id,
            from.format("%Y-%m-%d"),
            to.format("%Y-%m-%d")
        );

        let response = self.client.get(&url).send().await?;

        Ok(response.text().await?)
    }

    /// Retrieves all status pages from the BetterStack account
    ///
    /// # Returns
    ///
    /// * `Ok(String)` - JSON string containing all status pages data
    /// * `Err` - If the API request fails
    async fn get_status_pages(&self) -> Result<String, Box<dyn Error>> {
        // Status pages API uses betteruptime.com instead of uptime.betterstack.com
        let status_page_uri = self
            .api_uri
            .replace("uptime.betterstack.com", "betteruptime.com");
        let response = self
            .client
            .get(format!("{status_page_uri}/status-pages"))
            .send()
            .await?;

        Ok(response.text().await?)
    }

    /// Retrieves all resources (monitors) for a specific status page
    ///
    /// # Arguments
    ///
    /// * `status_page_id` - The ID of the status page
    ///
    /// # Returns
    ///
    /// * `Ok(String)` - JSON string containing the status page resources
    /// * `Err` - If the API request fails
    async fn get_status_page_resources(
        &self,
        status_page_id: &str,
    ) -> Result<String, Box<dyn Error>> {
        let mut all_resources = Vec::new();
        let mut page = 1;

        loop {
            let response = self
                .client
                .get(format!(
                    "{}/status-pages/{}/resources?page={}",
                    self.api_uri, status_page_id, page
                ))
                .send()
                .await?;

            let response_text = response.text().await?;
            let response_json: Value = serde_json::from_str(&response_text)?;

            // Extract resources from this page
            if let Some(data) = response_json["data"].as_array() {
                all_resources.extend(data.clone());
            }

            // Check if there's a next page
            if response_json["pagination"]["next"].is_null() {
                break;
            }

            page += 1;
        }

        // Construct the final response with all resources
        let final_response = serde_json::json!({
            "data": all_resources
        });

        Ok(final_response.to_string())
    }

    /// Retrieves all monitor IDs that are published on status pages
    ///
    /// # Returns
    ///
    /// * `Ok(Vec<String>)` - Vector of monitor IDs that are on status pages
    /// * `Err` - If the API request fails
    async fn get_status_page_monitors(&self) -> Result<Vec<String>, Box<dyn Error>> {
        let mut monitor_ids = Vec::new();

        // Get all status pages
        let status_pages: Value = serde_json::from_str(&self.get_status_pages().await?)?;

        if let Some(pages) = status_pages["data"].as_array() {
            for page in pages {
                if let Some(page_id) = page["id"].as_str() {
                    // Get resources for this status page
                    let resources: Value =
                        serde_json::from_str(&self.get_status_page_resources(page_id).await?)?;

                    if let Some(resources_data) = resources["data"].as_array() {
                        for resource in resources_data {
                            // Check if this resource is a monitor
                            if let Some(resource_type) =
                                resource["attributes"]["resource_type"].as_str()
                            {
                                if resource_type == "Monitor" {
                                    // Resource ID can be either string or number
                                    if let Some(monitor_id) =
                                        resource["attributes"]["resource_id"].as_str()
                                    {
                                        monitor_ids.push(monitor_id.to_string());
                                    } else if let Some(monitor_id) =
                                        resource["attributes"]["resource_id"].as_u64()
                                    {
                                        monitor_ids.push(monitor_id.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Remove duplicates in case a monitor appears on multiple status pages
        monitor_ids.sort();
        monitor_ids.dedup();

        Ok(monitor_ids)
    }

    /// Calculates uptime statistics for a specific monitor
    ///
    /// # Arguments
    ///
    /// * `monitor_id` - The ID of the monitor
    /// * `monitor_name` - The name of the monitor
    /// * `from` - Start date for the uptime calculation
    /// * `to` - End date for the uptime calculation
    ///
    /// # Returns
    ///
    /// A HashMap containing:
    /// * `id` - Monitor ID
    /// * `name` - Monitor name
    /// * `percentage` - Uptime percentage
    /// * `downtime` - Total downtime in seconds
    /// * `downtime_mins` - Total downtime in minutes
    /// * `uptime` - Total uptime in seconds
    /// * `max_uptime` - Total monitored time in seconds
    async fn calculate_uptime(
        &self,
        monitor_id: &str,
        monitor_name: &str,
        from: &DateTime<Utc>,
        to: &DateTime<Utc>,
    ) -> Result<HashMap<String, Value>, Box<dyn Error>> {
        let mut uptime_calc = HashMap::new();
        uptime_calc.insert("id".to_string(), Value::String(monitor_id.to_string()));
        uptime_calc.insert("name".to_string(), Value::String(monitor_name.to_string()));
        uptime_calc.insert("uptime".to_string(), Value::Number(0.into()));
        uptime_calc.insert("downtime".to_string(), Value::Number(0.into()));
        uptime_calc.insert("unmonitored".to_string(), Value::Number(0.into()));
        uptime_calc.insert("max_uptime".to_string(), Value::Number(0.into()));
        uptime_calc.insert(
            "percentage".to_string(),
            Value::Number(serde_json::Number::from_f64(0.0).unwrap()),
        );
        uptime_calc.insert("downtime_mins".to_string(), Value::Number(0.into()));

        let monitor_sla: Value =
            serde_json::from_str(&self.get_monitor_sla(monitor_id, from, to).await?)?;

        // Check if we have valid data in the response
        if let Some(attributes) = monitor_sla["data"]["attributes"].as_object() {
            // BetterStack returns availability as a percentage (e.g., 99.95)
            let availability = attributes["availability"].as_f64().unwrap_or(0.0);
            let total_downtime_secs = attributes["total_downtime"].as_u64().unwrap_or(0);
            let downtime_mins = total_downtime_secs / 60;

            uptime_calc.insert(
                "percentage".to_string(),
                Value::Number(serde_json::Number::from_f64(availability).unwrap()),
            );
            uptime_calc.insert(
                "downtime".to_string(),
                Value::Number(total_downtime_secs.into()),
            );
            uptime_calc.insert(
                "downtime_mins".to_string(),
                Value::Number(downtime_mins.into()),
            );

            // Calculate uptime from availability percentage and time range
            let total_seconds = (to.timestamp() - from.timestamp()) as u64;
            let uptime_seconds = (total_seconds as f64 * (availability / 100.0)) as u64;
            uptime_calc.insert("uptime".to_string(), Value::Number(uptime_seconds.into()));
            uptime_calc.insert(
                "max_uptime".to_string(),
                Value::Number(total_seconds.into()),
            );
        }

        Ok(uptime_calc)
    }
}

/// Parses a date string into a UTC DateTime
///
/// Supports two formats:
/// * MM/DD/YYYY (e.g., 01/31/2024)
/// * MM-DD-YYYY (e.g., 01-31-2024)
///
/// # Arguments
///
/// * `date_str` - The date string to parse
///
/// # Returns
///
/// * `Ok(DateTime<Utc>)` - The parsed date at midnight UTC
/// * `Err` - If the date string doesn't match either supported format
fn parse_date(date_str: &str) -> Result<DateTime<Utc>, Box<dyn Error>> {
    // Try parsing with MM/DD/YYYY format first
    if let Ok(naive_date) = NaiveDate::parse_from_str(date_str, "%m/%d/%Y") {
        return Ok(Utc.from_utc_datetime(&naive_date.and_hms_opt(0, 0, 0).unwrap()));
    }

    // If that fails, try MM-DD-YYYY format
    let naive_date = NaiveDate::parse_from_str(date_str, "%m-%d-%Y")?;
    Ok(Utc.from_utc_datetime(&naive_date.and_hms_opt(0, 0, 0).unwrap()))
}

/// Main entry point for the BetterStack uptime calculator
///
/// This function:
/// 1. Parses command line arguments for date range
/// 2. Loads API credentials from environment variables
/// 3. Fetches all monitors from BetterStack
/// 4. Calculates uptime statistics for each monitor concurrently
/// 5. Outputs results sorted by monitor name
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    dotenv().ok(); // Load .env file if it exists

    let args = Args::parse();

    let (start_date, end_date) = if args.start_date.is_none() || args.end_date.is_none() {
        // Default to last year from today
        let end_date = Utc::now();
        let start_date = end_date - chrono::Duration::days(365);
        (start_date, end_date)
    } else {
        let start = parse_date(&args.start_date.unwrap())?;
        let end = parse_date(&args.end_date.unwrap())?;
        (start, end)
    };

    println!(
        "Calculating uptime from {} to {}",
        start_date.format("%Y-%m-%d"),
        end_date.format("%Y-%m-%d")
    );

    let api_token = env::var("BETTERSTACK_API_TOKEN")
        .expect("BETTERSTACK_API_TOKEN must be set in environment or .env file");
    let api_url = env::var("BETTERSTACK_API_URL")
        .unwrap_or_else(|_| "https://uptime.betterstack.com/api/v2".to_string());
    let betterstack_api = BetterStackApi::new(&api_token, &api_url);
    let all_monitors: Value = serde_json::from_str(&betterstack_api.get_monitors().await?)?;

    // Filter monitors based on status page flag
    let monitors_to_process = if args.status_page_only {
        println!("Fetching monitors from status pages...");
        let status_page_monitor_ids = betterstack_api.get_status_page_monitors().await?;

        // Filter all monitors to only include those on status pages
        let filtered_monitors: Vec<Value> = all_monitors["data"]
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter(|monitor| {
                if let Some(monitor_id) = monitor["id"].as_str() {
                    status_page_monitor_ids.contains(&monitor_id.to_string())
                } else {
                    false
                }
            })
            .cloned()
            .collect();

        println!(
            "Found {} monitors on status pages out of {} total monitors",
            filtered_monitors.len(),
            all_monitors["data"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0)
        );

        filtered_monitors
    } else {
        let monitor_count = all_monitors["data"]
            .as_array()
            .map(|a| a.len())
            .unwrap_or(0);
        println!("Processing {monitor_count} monitors...");

        all_monitors["data"].as_array().unwrap_or(&vec![]).clone()
    };

    let uptime_calculations = stream::iter(&monitors_to_process)
        .map(|m| {
            let betterstack_api = betterstack_api.clone();
            let monitor_id = m["id"].as_str().unwrap_or("").to_string();
            let monitor_name = m["attributes"]["pronounceable_name"]
                .as_str()
                .or(m["attributes"]["url"].as_str())
                .unwrap_or("Unknown")
                .to_string();
            async move {
                let result = betterstack_api
                    .calculate_uptime(&monitor_id, &monitor_name, &start_date, &end_date)
                    .await;
                sleep(Duration::from_millis(200)).await; // Add a small delay to avoid rate limiting
                result
            }
        })
        .buffer_unordered(10) // Process up to 10 requests concurrently
        .collect::<Vec<_>>()
        .await;

    let mut uptime_calculations: Vec<_> = uptime_calculations
        .into_iter()
        .filter_map(Result::ok)
        .collect();

    // Apply search filter if provided
    if !args.search.is_empty() {
        let search_terms = &args.search;
        uptime_calculations.retain(|monitor| {
            if let Some(name) = monitor["name"].as_str() {
                let name_lower = name.to_lowercase();
                search_terms
                    .iter()
                    .any(|term| name_lower.contains(&term.to_lowercase()))
            } else {
                false
            }
        });

        // Check if we have any results after filtering
        if uptime_calculations.is_empty() {
            println!(
                "\nNo monitors found matching any of the search terms: {}",
                search_terms.join(", ")
            );
            return Ok(());
        }

        println!(
            "\nFound {} monitors matching any of the search terms: {}",
            uptime_calculations.len(),
            search_terms.join(", ")
        );
    }

    // Sort monitors: 100% uptime first (alphabetically), then <100% from best to worst
    uptime_calculations.sort_by(|a, b| {
        let a_percentage = a["percentage"].as_f64().unwrap_or(0.0);
        let b_percentage = b["percentage"].as_f64().unwrap_or(0.0);

        match (a_percentage == 100.0, b_percentage == 100.0) {
            // Both are 100% - sort alphabetically by name
            (true, true) => a["name"].to_string().cmp(&b["name"].to_string()),
            // One is 100%, other is not - 100% comes first
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            // Both are less than 100% - sort by percentage (highest first)
            (false, false) => b_percentage
                .partial_cmp(&a_percentage)
                .unwrap_or(std::cmp::Ordering::Equal),
        }
    });

    // Calculate column widths
    let mut max_name_len = "Monitor Name".len();
    for u in &uptime_calculations {
        let name_len = u["name"].as_str().unwrap_or("").len();
        if name_len > max_name_len {
            max_name_len = name_len;
        }
    }

    // Print table header
    println!("\n{}", "=".repeat(max_name_len + 30));
    println!(
        "{:<width$} | {:>8} | {:>12}",
        "Monitor Name",
        "Uptime %",
        "Downtime",
        width = max_name_len
    );
    println!("{}", "=".repeat(max_name_len + 30));

    // Calculate average uptime
    let total_uptime: f64 = uptime_calculations
        .iter()
        .map(|u| u["percentage"].as_f64().unwrap_or(0.0))
        .sum();
    let average_uptime = if !uptime_calculations.is_empty() {
        total_uptime / uptime_calculations.len() as f64
    } else {
        0.0
    };

    // Print all monitors with 100% uptime first
    let mut printed_separator = false;
    for u in &uptime_calculations {
        let percentage = u["percentage"].as_f64().unwrap_or(0.0);
        let name = u["name"].as_str().unwrap_or("");
        let downtime_mins = u["downtime_mins"].as_i64().unwrap_or(0);

        // Print separator before first monitor with <100% uptime
        if percentage < 100.0 && !printed_separator {
            println!("{}", "-".repeat(max_name_len + 30));
            println!("Monitors with downtime (best to worst):");
            println!("{}", "-".repeat(max_name_len + 30));
            printed_separator = true;
        }

        // Format percentage based on whether it's 100% or not
        if percentage == 100.0 {
            println!(
                "{:<width$} | {:>7}% | {:>10} mins",
                name,
                "100",
                downtime_mins,
                width = max_name_len
            );
        } else {
            println!(
                "{:<width$} | {:>7.3}% | {:>10} mins",
                name,
                percentage,
                downtime_mins,
                width = max_name_len
            );
        }
    }
    println!("{}", "=".repeat(max_name_len + 30));

    // Display average uptime with conditional formatting
    if average_uptime == 100.0 {
        println!("\nAverage Uptime: 100%");
    } else {
        println!("\nAverage Uptime: {average_uptime:.4}%");
    }
    println!("Total Monitors: {}", uptime_calculations.len());

    Ok(())
}
