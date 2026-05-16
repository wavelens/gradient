# socket.rs extraction map

## Move to proto::session::frame
- ProtoSocket, ProtoReader, ProtoWriter (enums + impls)
- recv_client_msg, send_server_msg, send_error
- axum_writer_task, tungstenite_writer_task
- NAR_PUSH_CHUNK_SIZE, MAX_PROTO_MESSAGE_SIZE, HANDSHAKE_TIMEOUT, JOB_OFFER_CHUNK_SIZE

## Stays in handler::socket (moves to web in Task 12/17)
- push_pending_candidates (uses Scheduler)
- serve_nar_request (uses ServerState)
- invalidate_cached_path (uses ServerState)
- send_nar_unavailable (helper)
- send_credentials_for_job (uses ServerState + Scheduler)
- send_ssh_key_credential (uses ServerState)
