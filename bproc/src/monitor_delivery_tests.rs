//! Regression checks for the monitoring channel's lossless, bounded shutdown contract.
#[cfg(test)]
mod delivery_tests {
    use std::sync::mpsc as std_mpsc;
    use std::time::Duration;
    use tokio::sync::mpsc;

    #[test]
    fn bounded_delivery_preserves_every_sample_in_order() {
        let (sender, mut receiver) = mpsc::channel(2);
        let producer = std::thread::spawn(move || {
            for sample in 0..100 {
                sender.blocking_send(sample).unwrap();
            }
        });
        let mut samples = Vec::new();
        while let Some(sample) = receiver.blocking_recv() {
            samples.push(sample);
        }
        producer.join().unwrap();
        assert_eq!(samples, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn closing_full_receiver_unblocks_monitor_before_join() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender.blocking_send(0).unwrap();
        let (started, waiting) = std_mpsc::channel();
        let (finished, done) = std_mpsc::channel();
        let producer = std::thread::spawn(move || {
            started.send(()).unwrap();
            let disconnected = sender.blocking_send(1).is_err();
            finished.send(disconnected).unwrap();
        });
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        receiver.close();
        assert!(done.recv_timeout(Duration::from_secs(2)).unwrap());
        producer.join().unwrap();
    }
}
