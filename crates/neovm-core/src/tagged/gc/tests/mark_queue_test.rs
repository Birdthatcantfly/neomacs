use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

fn word(index: usize) -> MarkWord {
    MarkWord::of(TaggedValue::fixnum(index as i64))
}

#[test]
fn mark_queue_pending_tracks_empty_single_and_chunk_boundaries() {
    for count in [0, 1, 255, 256, 257] {
        let queue = MarkQueue::default();
        assert_eq!(queue.pending.load(Ordering::Acquire), 0);
        {
            let mut words = queue.lock().unwrap();
            words.extend((0..count).map(word));
        }
        assert_eq!(queue.pending.load(Ordering::Acquire), count);
        assert_eq!(
            queue.take_all().unwrap(),
            (0..count).map(word).collect::<Vec<_>>()
        );
        assert_eq!(queue.pending.load(Ordering::Acquire), 0);
        assert!(queue.take_all().unwrap().is_empty());
    }
}

#[test]
fn mark_queue_guard_publishes_clear_and_drain_lengths() {
    let queue = MarkQueue::default();
    queue.lock().unwrap().extend((0..257).map(word));
    assert_eq!(queue.pending.load(Ordering::Acquire), 257);
    queue.lock().unwrap().drain(..256).for_each(drop);
    assert_eq!(queue.pending.load(Ordering::Acquire), 1);
    queue.lock().unwrap().clear();
    assert_eq!(queue.pending.load(Ordering::Acquire), 0);
    assert!(queue.take_all().unwrap().is_empty());
}

#[test]
fn mark_queue_empty_drain_does_not_acquire_words_mutex() {
    let queue = Arc::new(MarkQueue::default());
    let held = queue.words.lock().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let marker_queue = Arc::clone(&queue);
    let marker = std::thread::spawn(move || {
        send.send(marker_queue.take_all().unwrap().len()).unwrap();
    });
    let completed = receive.recv_timeout(Duration::from_secs(10));
    drop(held);
    marker.join().unwrap();
    assert_eq!(completed.expect("empty drain locked the shared words"), 0);
}

#[test]
fn mark_queue_poison_is_reported_even_with_zero_pending() {
    for count in [0, 1, 257] {
        let queue = MarkQueue::default();
        let panic = std::panic::catch_unwind(|| {
            let mut words = queue.lock().unwrap();
            words.extend((0..count).map(word));
            panic!("poison SATB queue after mutation");
        });
        assert!(panic.is_err());
        assert!(queue.is_poisoned());
        assert_eq!(queue.pending.load(Ordering::Acquire), count);
        let poison = queue.take_all().unwrap_err();
        let words = poison.into_inner();
        assert_eq!(words.len(), count, "poison must retain every queued word");
        drop(words);
        assert!(queue.is_poisoned());
    }
}

#[test]
fn mark_queue_producers_and_marker_deliver_each_word_once() {
    const PRODUCERS: usize = 4;
    const WORDS: usize = 4097;
    let queue = Arc::new(MarkQueue::default());
    let completed = Arc::new(AtomicUsize::new(0));
    let mut producers = Vec::new();
    for producer in 0..PRODUCERS {
        let queue = Arc::clone(&queue);
        let completed = Arc::clone(&completed);
        producers.push(std::thread::spawn(move || {
            for index in 0..WORDS {
                queue.lock().unwrap().push(word(producer * WORDS + index));
                if index % 256 == 0 {
                    std::thread::yield_now();
                }
            }
            completed.fetch_add(1, Ordering::Release);
        }));
    }
    let mut received = Vec::with_capacity(PRODUCERS * WORDS);
    loop {
        received.extend(queue.take_all().unwrap());
        if completed.load(Ordering::Acquire) == PRODUCERS {
            received.extend(queue.take_all().unwrap());
            break;
        }
        std::thread::yield_now();
    }
    for producer in producers {
        producer.join().unwrap();
    }
    received.sort_by_key(|word| word.0);
    let mut expected: Vec<_> = (0..PRODUCERS * WORDS).map(word).collect();
    expected.sort_by_key(|word| word.0);
    assert_eq!(received, expected);
    assert_eq!(queue.pending.load(Ordering::Acquire), 0);
    assert!(queue.take_all().unwrap().is_empty());
}
