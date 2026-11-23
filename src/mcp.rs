use crate::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::Duration;

const MAX_REQUEST: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Button {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Move {
        x: i32,
        y: i32,
    },
    Click {
        x: i32,
        y: i32,
        #[serde(default)]
        button: Button,
    },
    DoubleClick {
        x: i32,
        y: i32,
        #[serde(default)]
        button: Button,
    },
    Drag {
        path: Vec<Point>,
        #[serde(default)]
        button: Button,
    },
    Scroll {
        x: i32,
        y: i32,
        #[serde(default)]
        scroll_x: i32,
        #[serde(default)]
        scroll_y: i32,
    },
    Keypress {
        keys: Vec<String>,
    },
    #[serde(rename = "type")]
    Text {
        text: String,
    },
    Wait {
        milliseconds: u32,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Batch {
    pub actions: Vec<Action>,
    #[serde(default)]
    pub narration: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
}

pub struct Request {
    pub command: Command,
    pub reply: SyncSender<std::result::Result<Vec<u8>, String>>,
}

pub enum Command {
    Screenshot,
    Act(Batch),
}

pub struct Server {
    listener: TcpListener,
    pub address: SocketAddr,
    pub token: String,
}

impl Server {
    pub fn bind() -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let address = listener.local_addr()?;
        let mut secret = [0; 32];
        getrandom::fill(&mut secret)
            .map_err(|err| format!("Generating a tool session token: {err}"))?;
        Ok(Self {
            listener,
            address,
            token: STANDARD.encode(secret),
        })
    }

    pub fn start(self, turns: u32, reference: Option<PathBuf>) -> Result<Receiver<Request>> {
        self.listener.set_nonblocking(true)?;
        let (send, receive) = mpsc::sync_channel(1);
        thread::spawn(move || {
            // Bound the lifetime if the harness exits before connecting.
            for _ in 0..600 {
                match self.listener.accept() {
                    Ok((connection, _)) => {
                        if let Err(error) =
                            self.serve(connection, &send, turns, reference.as_deref())
                        {
                            eprintln!("Paintrs MCP: {error}");
                        }
                        break;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(error) => {
                        eprintln!("Paintrs MCP: {error}");
                        break;
                    }
                }
            }
        });
        Ok(receive)
    }

    fn serve(
        &self,
        mut connection: TcpStream,
        send: &SyncSender<Request>,
        turns: u32,
        reference: Option<&Path>,
    ) -> Result<()> {
        connection.set_nonblocking(false)?;
        connection.set_read_timeout(Some(Duration::from_secs(3)))?;
        connection.set_write_timeout(Some(Duration::from_secs(30)))?;
        let mut reader = BufReader::new(connection.try_clone()?);
        let mut line = String::new();
        reader.by_ref().take(128).read_line(&mut line)?;
        if line.trim_end() != self.token {
            return Err("Invalid tool session token".into());
        }
        connection.set_read_timeout(None)?;

        let mut remaining = turns;
        loop {
            line.clear();
            if !read_request(&mut reader, &mut line)? {
                return Ok(());
            }
            let message: Value = serde_json::from_str(&line)?;
            let Some(id) = message.get("id") else {
                continue;
            };
            let params = &message["params"];
            let result = match message["method"].as_str() {
                Some("initialize") => json!({
                    "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "paintrs", "version": env!("CARGO_PKG_VERSION")}
                }),
                Some("ping") => json!({}),
                Some("tools/list") => json!({"tools": tools(turns > 0, reference.is_some())}),
                Some("tools/call") => match (params["name"].as_str(), reference) {
                    (Some("reference"), Some(path)) => match reference_image(path) {
                        Ok(image) => image,
                        Err(error) => tool_error(&error.to_string()),
                    },
                    _ => match call(send, params, &mut remaining) {
                        Ok(image) => image_response(&image, "image/png"),
                        Err(error) => tool_error(&error),
                    },
                },
                _ => {
                    writeln!(
                        connection,
                        "{}",
                        json!({
                            "jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "Method not found"}
                        })
                    )?;
                    continue;
                }
            };
            writeln!(
                connection,
                "{}",
                json!({"jsonrpc":"2.0","id":id,"result": result})
            )?;
        }
    }
}

fn reference_image(path: &Path) -> Result<Value> {
    let media_type = crate::reference_media_type(path).ok_or("Unsupported reference image")?;
    Ok(image_response(&std::fs::read(path)?, media_type))
}

fn image_response(image: &[u8], media_type: &str) -> Value {
    json!({"content": [{"type": "image", "data": STANDARD.encode(image), "mimeType": media_type}]})
}

fn tool_error(error: &str) -> Value {
    json!({"isError": true, "content": [{"type": "text", "text": error}]})
}

fn call(
    send: &SyncSender<Request>,
    params: &Value,
    remaining: &mut u32,
) -> std::result::Result<Vec<u8>, String> {
    let command = match params["name"].as_str() {
        Some("screenshot") => Command::Screenshot,
        Some("act") => Command::Act(
            serde_json::from_value(params["arguments"].clone())
                .map_err(|error| format!("Invalid actions: {error}"))?,
        ),
        _ => return Err("Unknown Paintrs tool".into()),
    };
    if *remaining == 0 {
        return Err("Reached the drawing step limit. Finish drawing.".into());
    }
    *remaining -= 1;
    let (reply, result) = mpsc::sync_channel(1);
    send.send(Request { command, reply })
        .map_err(|_| "The drawing session has ended")?;
    result
        .recv_timeout(Duration::from_secs(50))
        .map_err(|_| "The drawing session did not respond")?
}

fn tools(drawing: bool, reference: bool) -> Vec<Value> {
    let mut tools = if drawing {
        vec![
            json!({
                "name": "screenshot",
                "description": "See the entire Paintrs window. The coordinate origin is the top left.",
                "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
                "annotations": {"readOnlyHint": true}
            }),
            json!({
                "name": "act",
                "description": "Perform mouse and keyboard actions in Paintrs and see the result. Clicks and moves take x and y. Drags take a path of x/y points. Keypress takes keys like ['CTRL','Z']. Type takes text. Scroll takes x, y and scroll_x or scroll_y. Wait takes milliseconds. For a polygon, choose the Polygon tool to the right of Rectangle. Drag the first edge only, then click each remaining vertex once and press Enter to close the shape. A single drag around the outline does not add vertices. Choose the bottom fill option before drawing for a solid foreground-color polygon.",
                "inputSchema": {
                    "type": "object", "required": ["actions"], "additionalProperties": false,
                    "properties": {
                        "actions": {"type": "array", "minItems": 1, "maxItems": 128, "items": {
                            "type": "object", "required": ["type"], "additionalProperties": false,
                            "properties": {
                                "type": {"enum": ["move","click","double_click","drag","scroll","keypress","type","wait"]},
                                "x": {"type": "integer"}, "y": {"type": "integer"},
                                "button": {"enum": ["left", "right"]},
                                "path": {"type": "array", "minItems": 2, "maxItems": 128, "items": {
                                    "type": "object", "required": ["x", "y"], "additionalProperties": false,
                                    "properties": {"x": {"type": "integer"}, "y": {"type": "integer"}}
                                }},
                                "scroll_x": {"type": "integer"}, "scroll_y": {"type": "integer"},
                                "keys": {"type": "array", "items": {"type": "string"}},
                                "text": {"type": "string"}, "milliseconds": {"type": "integer"}
                            }
                        }},
                        "narration": {"type": "string", "maxLength": 200, "description": "One short first-person line about what you're doing, including how you're fixing any mistake the screenshot or tool result showed."},
                        "subject": {"type": "string", "maxLength": 200, "description": "In the first drawing batch, name the specific subject you intend to draw."}
                    }
                }
            }),
        ]
    } else {
        Vec::new()
    };
    if reference {
        tools.push(json!({
            "name": "reference",
            "description": "See the image provided for this task.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true}
        }));
    }
    tools
}

fn read_request(reader: &mut impl BufRead, line: &mut String) -> Result<bool> {
    let size = reader.take(MAX_REQUEST + 1).read_line(line)? as u64;
    if size > MAX_REQUEST {
        return Err("Paintrs MCP message exceeds 1 MiB".into());
    }
    Ok(size != 0)
}

pub fn relay(address: SocketAddr) -> Result<()> {
    if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) {
        return Err("Paintrs MCP uses the loopback interface".into());
    }
    let token = std::env::var("AUTO_PAINT_MCP_TOKEN")?;
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_secs(10))?;
    writeln!(socket, "{token}")?;
    let mut replies = BufReader::new(socket.try_clone()?);
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut line = String::new();
    while read_request(&mut input, &mut line)? {
        let has_reply = serde_json::from_str::<Value>(&line)?.get("id").is_some();
        socket.write_all(line.as_bytes())?;
        line.clear();
        if has_reply {
            if replies.read_line(&mut line)? == 0 {
                return Err("Paintrs MCP closed while a response was pending".into());
            }
            output.write_all(line.as_bytes())?;
            output.flush()?;
            line.clear();
        }
    }
    Ok(())
}
