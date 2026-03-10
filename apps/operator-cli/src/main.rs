//! Operator CLI: submit jobs and check status via the API gateway.
//!
//! Usage:
//!   operator-cli submit <file.json>
//!   operator-cli status <job-id>
//!   operator-cli detail <job-id>
//!   operator-cli logs   <job-id>
//!   operator-cli retry  <job-id>
//!   operator-cli delete <job-id>
//!   operator-cli list
//!   operator-cli health

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pipeline_types::PipelineJob;

#[derive(Parser)]
#[command(name = "operator-cli", about = "Media pipeline operator CLI")]
struct Cli {
    /// API gateway base URL
    #[arg(long, env = "API_URL", default_value = "http://127.0.0.1:3190")]
    api_url: String,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Submit a new pipeline job from a JSON file
    Submit { file: String },
    /// Check the status of a job
    Status { job_id: String },
    /// Show detailed job info including stage outputs
    Detail { job_id: String },
    /// Show event log for a job
    Logs { job_id: String },
    /// Retry a failed job from its current stage
    Retry { job_id: String },
    /// Delete a job and all its data
    Delete { job_id: String },
    /// List all jobs
    List,
    /// Show service health dashboard from orchestrator
    Health,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();

    match cli.cmd {
        Command::Submit { file } => submit_job(&client, &cli.api_url, &file).await?,
        Command::Status { job_id } => get_status(&client, &cli.api_url, &job_id).await?,
        Command::Detail { job_id } => get_detail(&client, &cli.api_url, &job_id).await?,
        Command::Logs { job_id } => get_logs(&client, &cli.api_url, &job_id).await?,
        Command::Retry { job_id } => retry_job(&client, &cli.api_url, &job_id).await?,
        Command::Delete { job_id } => delete_job(&client, &cli.api_url, &job_id).await?,
        Command::List => list_jobs(&client, &cli.api_url).await?,
        Command::Health => show_health(&client).await?,
    }

    Ok(())
}

async fn submit_job(client: &reqwest::Client, api_url: &str, file: &str) -> Result<()> {
    let content =
        std::fs::read_to_string(file).with_context(|| format!("failed to read {file}"))?;
    let body: serde_json::Value =
        serde_json::from_str(&content).with_context(|| format!("invalid JSON in {file}"))?;

    let resp = client
        .post(format!("{api_url}/jobs"))
        .json(&body)
        .send()
        .await
        .context("failed to connect to API gateway")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("API returned {status}: {text}");
    }

    let job: PipelineJob = resp.json().await.context("failed to parse response")?;
    println!("Job created: {}", job.job_id);
    println!("  Title:  {}", job.request.title);
    println!("  Status: {}", job.status);
    println!("  Stage:  {}", job.current_stage);

    Ok(())
}

async fn get_status(client: &reqwest::Client, api_url: &str, job_id: &str) -> Result<()> {
    let resp = client
        .get(format!("{api_url}/jobs/{job_id}"))
        .send()
        .await
        .context("failed to connect to API gateway")?;

    if resp.status().as_u16() == 404 {
        anyhow::bail!("Job '{job_id}' not found");
    }
    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("API returned {status}");
    }

    let job: PipelineJob = resp.json().await.context("failed to parse response")?;
    println!("Job:    {}", job.job_id);
    println!("Title:  {}", job.request.title);
    println!("Status: {}", job.status);
    println!("Stage:  {}", job.current_stage);
    println!("Submitted: {}", job.submitted_at);

    Ok(())
}

async fn get_detail(client: &reqwest::Client, api_url: &str, job_id: &str) -> Result<()> {
    let resp = client
        .get(format!("{api_url}/jobs/{job_id}/detail"))
        .send()
        .await
        .context("failed to connect to API gateway")?;

    if resp.status().as_u16() == 404 {
        anyhow::bail!("Job '{job_id}' not found");
    }
    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("API returned {status}");
    }

    let detail: serde_json::Value = resp.json().await.context("failed to parse response")?;

    println!("Job:       {}", detail["job_id"].as_str().unwrap_or("?"));
    println!(
        "Title:     {}",
        detail["request"]["title"].as_str().unwrap_or("?")
    );
    println!("Status:    {}", detail["status"].as_str().unwrap_or("?"));
    println!(
        "Stage:     {}",
        detail["current_stage"].as_str().unwrap_or("?")
    );
    println!(
        "Submitted: {}",
        detail["submitted_at"].as_str().unwrap_or("?")
    );

    if let Some(stages) = detail["completed_stages"].as_array() {
        if stages.is_empty() {
            println!("\nNo completed stages yet.");
        } else {
            println!("\nCompleted stages:");
            for stage in stages {
                if let Some(name) = stage.as_str() {
                    println!("  - {name}");
                }
            }
        }
    }

    if let Some(outputs) = detail["stage_outputs"].as_object()
        && !outputs.is_empty()
    {
        println!("\nStage outputs:");
        for (stage, output) in outputs {
            let summary = serde_json::to_string(output).unwrap_or_default();
            let summary = truncate(&summary, 120);
            println!("  {stage}: {summary}");
        }
    }

    Ok(())
}

async fn get_logs(client: &reqwest::Client, api_url: &str, job_id: &str) -> Result<()> {
    let resp = client
        .get(format!("{api_url}/jobs/{job_id}/events"))
        .send()
        .await
        .context("failed to connect to API gateway")?;

    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("API returned {status}");
    }

    let events: Vec<serde_json::Value> = resp.json().await.context("failed to parse response")?;

    if events.is_empty() {
        println!("No events recorded for job '{job_id}'.");
        return Ok(());
    }

    for event in &events {
        let ts = event["timestamp"].as_str().unwrap_or("?");
        let level = event["level"].as_str().unwrap_or("?");
        let msg = event["message"].as_str().unwrap_or("?");
        let level_tag = match level {
            "error" => "ERR ",
            "warn" => "WARN",
            _ => "INFO",
        };
        println!("{ts}  [{level_tag}]  {msg}");
    }
    println!("\n{} event(s)", events.len());

    Ok(())
}

async fn retry_job(client: &reqwest::Client, api_url: &str, job_id: &str) -> Result<()> {
    let resp = client
        .post(format!("{api_url}/jobs/{job_id}/retry"))
        .send()
        .await
        .context("failed to connect to API gateway")?;

    match resp.status().as_u16() {
        200 => println!("Job '{job_id}' re-enqueued for retry."),
        404 => anyhow::bail!("Job '{job_id}' not found"),
        409 => {
            anyhow::bail!("Job '{job_id}' is not in Failed state — only failed jobs can be retried")
        }
        code => anyhow::bail!("API returned {code}"),
    }
    Ok(())
}

async fn delete_job(client: &reqwest::Client, api_url: &str, job_id: &str) -> Result<()> {
    let resp = client
        .delete(format!("{api_url}/jobs/{job_id}"))
        .send()
        .await
        .context("failed to connect to API gateway")?;

    match resp.status().as_u16() {
        200 => println!("Job '{job_id}' deleted."),
        404 => anyhow::bail!("Job '{job_id}' not found"),
        code => anyhow::bail!("API returned {code}"),
    }
    Ok(())
}

async fn show_health(client: &reqwest::Client) -> Result<()> {
    // The health dashboard is on the orchestrator (port 3199 by default).
    // Derive orchestrator URL from api_url or use ORCHESTRATOR_URL env var.
    let orchestrator_url =
        std::env::var("ORCHESTRATOR_URL").unwrap_or_else(|_| "http://127.0.0.1:3199".into());

    let resp = client
        .get(format!("{orchestrator_url}/status"))
        .send()
        .await
        .context("failed to connect to orchestrator")?;

    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("Orchestrator returned {status}");
    }

    let status: serde_json::Value = resp.json().await.context("failed to parse response")?;

    println!(
        "Orchestrator: {} | Pending jobs: {}",
        if status["running"].as_bool().unwrap_or(false) {
            "running"
        } else {
            "stopped"
        },
        status["pending_jobs"].as_u64().unwrap_or(0),
    );

    if let Some(services) = status["services"].as_array() {
        println!("\n{:<16} {:<8} URL", "SERVICE", "STATUS");
        println!("{}", "-".repeat(70));
        for svc in services {
            let name = svc["name"].as_str().unwrap_or("?");
            let healthy = svc["healthy"].as_bool().unwrap_or(false);
            let url = svc["url"].as_str().unwrap_or("?");
            let tag = if healthy { "UP" } else { "DOWN" };
            println!("{:<16} {:<8} {url}", name, tag);
            if let Some(err) = svc["error"].as_str() {
                println!("{:<16} {err}", "");
            }
        }
    }

    Ok(())
}

async fn list_jobs(client: &reqwest::Client, api_url: &str) -> Result<()> {
    let resp = client
        .get(format!("{api_url}/jobs"))
        .send()
        .await
        .context("failed to connect to API gateway")?;

    if !resp.status().is_success() {
        let status = resp.status();
        anyhow::bail!("API returned {status}");
    }

    let jobs: Vec<PipelineJob> = resp.json().await.context("failed to parse response")?;

    if jobs.is_empty() {
        println!("No jobs found.");
        return Ok(());
    }

    println!("{:<38} {:<30} {:<12} STAGE", "JOB ID", "TITLE", "STATUS");
    println!("{}", "-".repeat(95));
    for job in &jobs {
        println!(
            "{:<38} {:<30} {:<12} {}",
            job.job_id,
            truncate(&job.request.title, 28),
            job.status,
            job.current_stage,
        );
    }
    println!("\n{} job(s) total", jobs.len());

    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_long() {
        let result = truncate("a very long title that exceeds", 15);
        assert!(result.len() <= 15);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn cli_parses() {
        // Verify CLI struct can parse without panicking
        let cli = Cli::try_parse_from(["operator-cli", "list"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_detail() {
        let cli = Cli::try_parse_from(["operator-cli", "detail", "job-123"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_logs() {
        let cli = Cli::try_parse_from(["operator-cli", "logs", "job-123"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_retry() {
        let cli = Cli::try_parse_from(["operator-cli", "retry", "job-123"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_delete() {
        let cli = Cli::try_parse_from(["operator-cli", "delete", "job-123"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn cli_parses_health() {
        let cli = Cli::try_parse_from(["operator-cli", "health"]);
        assert!(cli.is_ok());
    }
}
