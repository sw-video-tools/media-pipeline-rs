//! Operator CLI: submit jobs and check status via the API gateway.
//!
//! Usage:
//!   operator-cli submit <file.json>
//!   operator-cli status <job-id>
//!   operator-cli list

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use pipeline_types::PipelineJob;

#[derive(Parser)]
#[command(name = "operator-cli", about = "Media pipeline operator CLI")]
struct Cli {
    /// API gateway base URL
    #[arg(long, env = "API_URL", default_value = "http://127.0.0.1:3000")]
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
    /// List all jobs
    List,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let client = reqwest::Client::new();

    match cli.cmd {
        Command::Submit { file } => submit_job(&client, &cli.api_url, &file).await?,
        Command::Status { job_id } => get_status(&client, &cli.api_url, &job_id).await?,
        Command::List => list_jobs(&client, &cli.api_url).await?,
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
}
