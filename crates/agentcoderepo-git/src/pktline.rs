/// Wrap git's advertise-refs output in the smart HTTP response format.
///
/// Format: pkt-line service header, flush-pkt, then the raw git output.
/// See https://git-scm.com/docs/http-protocol#_smart_server_response
pub fn wrap_advertisement(service: &str, git_output: &[u8]) -> Vec<u8> {
    let pkt_header = format!("# service={service}\n");
    let pkt_len = pkt_header.len() + 4; // 4 hex digits for the length prefix itself
    let header_line = format!("{pkt_len:04x}{pkt_header}");

    let mut body = Vec::with_capacity(header_line.len() + 4 + git_output.len());
    body.extend_from_slice(header_line.as_bytes());
    body.extend_from_slice(b"0000"); // flush-pkt
    body.extend_from_slice(git_output);
    body
}
