// Test to verify unbounded channel doesn't deadlock with many rapid sends
use tokio::sync::mpsc;
use std::time::Duration;

#[tokio::test]
async fn test_unbounded_channel_no_deadlock() {
    // Create unbounded channel (matches our fix)
    let (tx, mut rx) = mpsc::unbounded_channel::<u32>();

    // Spawn forwarder task (simulates processor.rs:1271-1281)
    let forwarder = tokio::spawn(async move {
        let mut count = 0;
        while let Some(_msg) = rx.recv().await {
            count += 1;
            // Simulate slow processing
            tokio::time::sleep(Duration::from_micros(10)).await;
        }
        count
    });

    // Send 500 messages rapidly (simulates backend sending many progress updates)
    for i in 0..500 {
        // This should NEVER block with unbounded channel
        tx.send(i).unwrap();
    }

    // Drop sender to close channel
    drop(tx);

    // Wait for forwarder to drain
    let received = forwarder.await.unwrap();
    assert_eq!(received, 500, "Forwarder should receive all 500 messages");
}

#[tokio::test]
async fn test_bounded_channel_would_deadlock() {
    // This test demonstrates what WOULD happen with bounded channel
    let (tx, mut rx) = mpsc::channel::<u32>(100); // Bounded capacity

    // Spawn slow forwarder
    let forwarder = tokio::spawn(async move {
        let mut count = 0;
        while let Some(_msg) = rx.recv().await {
            count += 1;
            // Slow processing
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        count
    });

    // Try to send 500 messages rapidly
    // This WILL block after 100 messages with bounded channel + slow receiver
    // We use tokio::time::timeout to prevent actual deadlock in test
    let send_result = tokio::time::timeout(Duration::from_millis(500), async {
        for i in 0..500 {
            tx.send(i).await.unwrap();
        }
    }).await;

    // With bounded channel and slow receiver, this WILL timeout
    assert!(send_result.is_err(), "Bounded channel should timeout with slow receiver");

    drop(tx);
    let _ = forwarder.await;
}
