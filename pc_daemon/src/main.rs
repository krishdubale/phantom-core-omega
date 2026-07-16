// ============================================================================
// PhantomCore Omega — PC Daemon
// Remote compute offloading via ARM64-to-x86 JIT with speculative execution
//
// Listens on UDP :42069, translates ARM64 basic blocks to x86_64,
// executes them in a sandboxed memory model, and streams results back.
// ============================================================================

mod benchmark;
mod fec;
mod jit;
mod nack;
mod predictor;
mod protocol;
mod sandbox;

use benchmark::BenchmarkCollector;
use fec::{FecDecoder, FecEncoder};
use nack::NackTracker;
use predictor::LSTMPredictor;
use protocol::FLAG_NACK;
use sandbox::SessionState;

use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::signal;

const LISTEN_PORT: u16 = 42069;
const MAX_UDP_PAYLOAD: usize = 65536;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    log::info!("╔══════════════════════════════════════════╗");
    log::info!("║     PhantomCore Omega — PC Daemon        ║");
    log::info!("║  ARM64→x86 JIT · LSTM Predictor · FEC    ║");
    log::info!("╚══════════════════════════════════════════╝");

    // Bind the UDP socket
    let socket = Arc::new(UdpSocket::bind(format!("0.0.0.0:{}", LISTEN_PORT)).await?);
    log::info!("Listening on UDP port {}", LISTEN_PORT);

    // Shared state
    let session_state = Arc::new(Mutex::new(SessionState::new()));
    let benchmark = Arc::new(Mutex::new(BenchmarkCollector::new()));
    let fec_encoder = Arc::new(Mutex::new(FecEncoder::new()));
    let _fec_decoder = Arc::new(Mutex::new(FecDecoder::new()));
    let nack_tracker = Arc::new(Mutex::new(NackTracker::new()));

    // Load LSTM predictor
    let predictor = Arc::new(Mutex::new(
        LSTMPredictor::load("models/syscall_predictor.weights")
            .unwrap_or_else(|_| {
                log::warn!("Using random LSTM weights (no trained model found)");
                LSTMPredictor::load("nonexistent").unwrap()
            }),
    ));

    // Spawn NACK timer task — sends NACKs every 5ms for missing packets
    let nack_socket = Arc::clone(&socket);
    let nack_tracker_clone = Arc::clone(&nack_tracker);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(5));
        let last_peer: Option<std::net::SocketAddr> = None;

        loop {
            interval.tick().await;

            // Scope the mutex guard so it's dropped before the next .await
            let packet_to_send = {
                let mut tracker = nack_tracker_clone.lock().unwrap();
                if tracker.should_send_nack() {
                    let nack_data = tracker.build_nack_packet();
                    if let Some(addr) = last_peer {
                        let mut packet = Vec::with_capacity(protocol::HEADER_SIZE + nack_data.len());
                        use byteorder::{LittleEndian, WriteBytesExt};
                        packet.write_u32::<LittleEndian>(0).unwrap();
                        packet.write_u32::<LittleEndian>(0).unwrap();
                        packet.write_u32::<LittleEndian>(0).unwrap();
                        packet.write_u16::<LittleEndian>(FLAG_NACK).unwrap();
                        packet.write_u16::<LittleEndian>(nack_data.len() as u16).unwrap();
                        packet.extend_from_slice(&nack_data);
                        Some((packet, addr))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }; // MutexGuard dropped here

            if let Some((packet, addr)) = packet_to_send {
                let _ = nack_socket.send_to(&packet, addr).await;
                log::trace!("Sent NACK packet to {}", addr);
            }
        }
    });

    // Spawn periodic stats logger
    let bench_clone = Arc::clone(&benchmark);
    let state_clone = Arc::clone(&session_state);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let (stats_json, state_json) = {
                let stats = bench_clone.lock().unwrap().get_stats();
                let sj = state_clone.lock().unwrap().stats_json();
                (stats, sj)
            };
            log::info!(
                "Stats: {} requests, avg latency {:.1}µs, p99 {:.1}µs, throughput {:.0} rps",
                stats_json.total_requests,
                stats_json.avg_latency_us,
                stats_json.p99_latency_us,
                stats_json.avg_throughput_rps
            );
            log::info!("Session: {}", state_json);
        }
    });

    // Main receive loop
    let mut buf = vec![0u8; MAX_UDP_PAYLOAD];

    log::info!("Ready to receive offload requests. Waiting for Android proxy...");

    loop {
        tokio::select! {
            result = socket.recv_from(&mut buf) => {
                let (len, addr) = result?;
                let data = buf[..len].to_vec();
                let request_start = Instant::now();

                // Parse the request
                let req = match protocol::parse_request(&data) {
                    Ok(r) => r,
                    Err(e) => {
                        log::warn!("Failed to parse packet from {}: {}", addr, e);
                        continue;
                    }
                };

                // Handle NACK packets
                if req.flags & FLAG_NACK != 0 {
                    log::trace!("Received NACK from {}", addr);
                    continue;
                }

                // Record in NACK tracker
                nack_tracker.lock().unwrap().record_received(req.seq);

                // Process the request
                let socket_clone = Arc::clone(&socket);
                let state = Arc::clone(&session_state);
                let bench = Arc::clone(&benchmark);
                let pred = Arc::clone(&predictor);
                let fec_enc = Arc::clone(&fec_encoder);

                tokio::spawn(async move {
                    // Do all mutex work in a sync block, collect results,
                    // then do async sends AFTER dropping all guards.
                    let (response_bytes, parity_packets) = {
                        let mut locked_state = state.lock().unwrap();
                        let mut locked_bench = bench.lock().unwrap();

                        // 1. Check speculative cache
                        let response = if let Some(cached) = locked_state.check_speculative_cache(req.seq) {
                            locked_bench.record_speculative_lookup(true);
                            log::debug!("Speculative HIT seq={}", req.seq);
                            cached
                        } else {
                            locked_bench.record_speculative_lookup(false);

                            // 2. Check JIT cache
                            let had_cached_block = locked_state.get_jit_block(req.pc).is_some();
                            locked_bench.record_jit_lookup(had_cached_block);

                            // 3. Translate and execute
                            let result = jit::translate_and_execute_x86(&req, &mut locked_state);

                            // 4. Feed LSTM predictor and pre-execute predictions
                            if let Ok(mut pred_locked) = pred.lock() {
                                let predictions = pred_locked.predict_next(&req);
                                for p in predictions {
                                    let spec_result = jit::translate_and_execute_x86(&p, &mut locked_state);
                                    locked_state.store_speculative(p.seq, spec_result);
                                }
                            }

                            result
                        };

                        // 5. Delta compress against previous state
                        let delta = protocol::compute_delta(&response, &locked_state.register_shadow);

                        // 6. Serialize
                        let resp_bytes = protocol::serialize_response(&delta);

                        // 7. FEC parity
                        let parities = {
                            let mut fec = fec_enc.lock().unwrap();
                            fec.add_packet(&resp_bytes)
                        };

                        // 8. Record benchmark
                        locked_bench.record_request(request_start, Instant::now());

                        log::trace!(
                            "Processed seq={} func_id={} -> {} bytes in {:.1}µs",
                            req.seq, req.func_id, resp_bytes.len(),
                            request_start.elapsed().as_micros()
                        );

                        (resp_bytes, parities)
                    }; // ALL MutexGuards dropped here — now safe to .await

                    // Send FEC parity packets (async, after guards dropped)
                    if let Some(parities) = parity_packets {
                        for parity in parities {
                            let _ = socket_clone.send_to(&parity, addr).await;
                        }
                    }

                    // Send the response
                    let _ = socket_clone.send_to(&response_bytes, addr).await;
                });
            }
            _ = signal::ctrl_c() => {
                log::info!("Shutting down...");
                // Save benchmark data
                let bench = benchmark.lock().unwrap();
                if let Err(e) = bench.save_to_file("phantom_benchmark.json") {
                    log::error!("Failed to save benchmark data: {}", e);
                } else {
                    log::info!("Benchmark data saved to phantom_benchmark.json");
                }
                log::info!("Final stats:\n{}", bench.to_json());
                break;
            }
        }
    }

    Ok(())
}
