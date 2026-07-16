//! Simple async test to check basic functionality

use tonepoet_backend::integration::*;
use tokio::sync::mpsc;

#[tokio::main]
async fn main() {
    println!("=== SIMPLE ASYNC TEST ===");

    // Test basic tokio channel functionality
    let (tx, mut rx) = mpsc::channel::<ProgressUpdate>(10);

    // Create a simple progress update
    let update = ProgressUpdate {
        item_id: "test".to_string(),
        progress: 50.0,
        status: ConversionStatus::Processing {
            progress: 50.0,
            message: Some("Test message".to_string()),
            file_progress: None,
            phase: Some(ConversionPhase::Converting),
            phase_progress: Some(25.0),
        },
    };

    println!("Sending progress update...");
    tx.send(update).await.unwrap();
    drop(tx);

    println!("Receiving progress update...");
    if let Some(received) = rx.recv().await {
        println!(
            "✅ Received: {} at {:.1}%",
            received.item_id, received.progress
        );
        println!("✅ Basic tokio async/channel functionality works");
    } else {
        println!("❌ Failed to receive progress update");
    }
}
