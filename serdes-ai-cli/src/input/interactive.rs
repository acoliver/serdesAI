//! Interactive input backed by the live completion dropdown.

use std::io;
use std::io::Write;
use std::path::Path;

use crate::completion;

pub async fn get_input_with_completion(
    prompt: &str,
    _history_file: Option<&Path>,
) -> io::Result<Option<String>> {
    let prompt_owned = prompt.to_string();

    tokio::task::spawn_blocking(move || {
        if !prompt_owned.is_empty() {
            print!("{prompt_owned}");
            io::stdout().flush()?;
        }

        completion::read_input_with_completion()
    })
    .await
    .map_err(|e| io::Error::other(format!("input task failed: {e}")))?
}

pub async fn get_line(prompt: &str) -> io::Result<Option<String>> {
    get_input_with_completion(prompt, None).await
}

/// Get confirmation (yes/no) from user
pub async fn get_confirmation(prompt: &str) -> std::io::Result<bool> {
    let prompt = format!("{} [y/N]: ", prompt);

    tokio::task::spawn_blocking(move || {
        print!("{}", prompt);
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        let trimmed = input.trim().to_lowercase();
        Ok(trimmed == "y" || trimmed == "yes")
    })
    .await
    .map_err(|e| std::io::Error::other(format!("Task failed: {}", e)))?
}

/// Get selection from list of options
pub async fn get_selection(prompt: &str, options: &[String]) -> std::io::Result<String> {
    let prompt = prompt.to_string();
    let options = options.to_vec();

    tokio::task::spawn_blocking(move || {
        println!("{}", prompt);
        for (i, opt) in options.iter().enumerate() {
            println!("  {}. {}", i + 1, opt);
        }
        print!("Enter number (1-{}): ", options.len());
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if let Ok(num) = input.trim().parse::<usize>() {
            if num > 0 && num <= options.len() {
                return Ok(options[num - 1].clone());
            }
        }

        Ok(input.trim().to_string())
    })
    .await
    .map_err(|e| std::io::Error::other(format!("Task failed: {}", e)))?
}
