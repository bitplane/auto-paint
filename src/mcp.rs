use crate::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
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
}

pub struct Request {
    pub command: Command,
    pub reply: SyncSender<std::result::Result<Option<Vec<u8>>, String>>,
}

pub enum Command {
    Screenshot,
    Act(Batch),
    Review(String),
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

    pub fn start(self, turns: u32, narrate: bool) -> Result<Receiver<Request>> {
        self.listener.set_nonblocking(true)?;
        let (send, receive) = mpsc::sync_channel(1);
        thread::spawn(move || {
            // Bound the lifetime if the harness exits before connecting.
            for _ in 0..600 {
                match self.listener.accept() {
                    Ok((connection, _)) => {
                        if let Err(error) = self.serve(connection, &send, turns, narrate) {
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
        narrate: bool,
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
        connection.set_read_timeout(Some(Duration::from_secs(900)))?;

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
                Some("tools/list") => json!({"tools": tools(narrate)}),
                Some("tools/call") => {
                    let result = call(send, params, &mut remaining, narrate);
                    match result {
                        Ok(Some(image)) => json!({"content": [{
                            "type": "image", "data": STANDARD.encode(image), "mimeType": "image/png"
                        }]}),
                        Ok(None) => {
                            json!({"content": [{"type": "text", "text": "Final review recorded."}]})
                        }
                        Err(error) => {
                            json!({"isError": true, "content": [{"type": "text", "text": error}]})
                        }
                    }
                }
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

fn call(
    send: &SyncSender<Request>,
    params: &Value,
    remaining: &mut u32,
    narrate: bool,
) -> std::result::Result<Option<Vec<u8>>, String> {
    let command = match params["name"].as_str() {
        Some("screenshot") => Command::Screenshot,
        Some("act") => Command::Act(
            serde_json::from_value(params["arguments"].clone())
                .map_err(|error| format!("Invalid actions: {error}"))?,
        ),
        Some("review") if narrate => {
            let line = params["arguments"]["narration"]
                .as_str()
                .unwrap_or("")
                .trim();
            if !(1..=200).contains(&line.chars().count()) {
                return Err("The final review needs 1 to 200 characters".into());
            }
            Command::Review(line.to_owned())
        }
        _ => return Err("Unknown Paintrs tool".into()),
    };
    if !matches!(command, Command::Review(_)) {
        if *remaining == 0 {
            return Err("Reached the drawing step limit. Finish drawing.".into());
        }
        *remaining -= 1;
    }
    let (reply, result) = mpsc::sync_channel(1);
    send.send(Request { command, reply })
        .map_err(|_| "The drawing session has ended")?;
    result
        .recv_timeout(Duration::from_secs(50))
        .map_err(|_| "The drawing session did not respond")?
}

fn tools(narrate: bool) -> Vec<Value> {
    let mut tools = vec![
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
                    "narration": {"type": "string", "maxLength": 200, "description": "One short first-person line about what you're doing, including how you're fixing any mistake the screenshot or tool result showed."}
                }
            }
        }),
    ];
    if narrate {
        tools.push(json!({
            "name": "review",
            "description": "Finish the drawing and narrate one short, scathing sentence about a visible flaw in the final picture.",
            "inputSchema": {
                "type": "object", "required": ["narration"], "additionalProperties": false,
                "properties": {"narration": {"type": "string", "minLength": 1, "maxLength": 200}}
            }
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
