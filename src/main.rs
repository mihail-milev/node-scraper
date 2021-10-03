use std::process::Command;
use std::str;
use std::sync::Arc;
use async_std::sync::Mutex;
use async_std::task;
use futures::stream::{FuturesUnordered, StreamExt};
use std::thread::sleep;
use std::time::Duration;
use std::io::Cursor;
use futures::join;
use tiny_http::Response;

static CPU_USER_ID : &str = "cpu_user";
static CPU_IDLE_ID : &str = "cpu_idle";
static CPU_SYSTEM_ID : &str = "cpu_system";
static CPU_NICE_ID : &str = "cpu_nice";
static MEM_TOTAL_ID : &str = "mem_total";
static MEM_FREE_ID : &str = "mem_free";
static MEM_USED_ID : &str = "mem_used";
static MEM_BUFFERED_ID : &str = "mem_buffered";
static PROC_CPU_ID : &str = "proc_cpu";
static PROC_MEM_ID : &str = "proc_mem";
static NETSTAT_INFO_ID : &str = "netstat_info";

struct DataHolder {
    data: Vec<(String, f64)>,
}

impl DataHolder {
    pub fn new() -> DataHolder {
        return DataHolder {
            data: vec![],
        };
    }

    pub fn get_data(&self) -> &Vec<(String, f64)> {
        return &self.data;
    }

    pub fn push_data(&mut self, dt: (String, f64)) {
        self.data.push(dt);
    }

    pub fn clear_data(&mut self) {
        self.data.clear();
    }
}

fn send_influx_value(dh: Arc<Mutex<DataHolder>>, id: &'static str, params: Option<Vec<(&str, &str)>>, val: f64) {
    let mut comb : String;
    if let Some(params) = params {
        comb = params.iter().fold(String::new(), |acc, &(k,v)| acc + k + "=\"" + v + "\", ");
        comb.pop();
        comb.pop();
    } else {
        comb = String::new();
    }
    task::block_on(async move {
        let mut lock = dh.lock().await;
        let itm = format!("{}{{{}}}", id, comb);
        (*lock).push_data((itm, val));
    });
}

fn handle_cpu_line(line: &str, dh: Arc<Mutex<DataHolder>>) -> Result<(), String> {
    let parts = line.split(" ");
    let mut prev = "";
    for part in parts {
        if let Ok(val) = prev.trim().replace(",", ".").parse::<f64>() {
            match part.trim_matches(|c| c == ' ' || c == ',') {
                "us" => {
                    send_influx_value(dh.clone(), CPU_USER_ID, None, val);
                },
                "sy" => {
                    send_influx_value(dh.clone(), CPU_SYSTEM_ID, None, val);
                }
                "ni" => {
                    send_influx_value(dh.clone(), CPU_NICE_ID, None, val);
                }
                "id" => {
                    send_influx_value(dh.clone(), CPU_IDLE_ID, None, val);
                }
                _ => {},
            }
        }
        prev = part;
    }
    Ok(())
}

fn handle_mem_line(line: &str, dh: Arc<Mutex<DataHolder>>) -> Result<(), String> {
    let parts = line.split(" ");
    let mut prev = "";
    for part in parts {
        if let Ok(val) = prev.trim().replace(",", ".").parse::<f64>() {
            match part.trim_matches(|c| c == ' ' || c == ',') {
                "total" => {
                    send_influx_value(dh.clone(), MEM_TOTAL_ID, None, val);
                },
                "free" => {
                    send_influx_value(dh.clone(), MEM_FREE_ID, None, val);
                }
                "used" => {
                    send_influx_value(dh.clone(), MEM_USED_ID, None, val);
                }
                "buff/cache" => {
                    send_influx_value(dh.clone(), MEM_BUFFERED_ID, None, val);
                }
                _ => {},
            };
        }
        prev = part;
    }
    Ok(())    
}

fn handle_proc_line(line: &str, dh: Arc<Mutex<DataHolder>>) -> Result<(), String> {
    let line = line.trim();
    let line_items : Vec<&str> = line.split(" ").filter(|&v| v != "").collect();
    let mut labels : Vec<(&str, &str)> = vec![];
    labels.push(("pid", line_items[0]));
    labels.push(("user", line_items[1]));
    labels.push(("command", line_items[line_items.len()-1]));
    let cpu_val = match line_items[8].replace(",", ".").parse::<f64>() {
        Ok(v) => v,
        Err(e) => {
            return Err(format!("Unable to convert process CPU to float: {}", e));
        },
    };
    let mem_val = match line_items[5].replace(",", ".").parse::<f64>() {
        Ok(v) => v,
        Err(e) => {
            return Err(format!("Unable to convert process memory to float: {}", e));
        },
    };
    send_influx_value(dh.clone(), PROC_CPU_ID, Some(labels.clone()), cpu_val);
    send_influx_value(dh, PROC_MEM_ID, Some(labels), mem_val);
    Ok(())
}

async fn handle_line(line: &str, dh: Arc<Mutex<DataHolder>>) -> Result<(), String> {
    if line.starts_with("%Cpu(s):") {
        return handle_cpu_line(line, dh);
    } else if line.starts_with("MiB Mem :") {
        return handle_mem_line(line, dh);
    } else {
        if line.len() > 0 {
            let first_char = line.trim().chars().next().unwrap();
            if &line[..1] == " " && first_char.is_ascii_digit() {
                return handle_proc_line(line, dh);
            }
        }
    }
    Ok(())
}

async fn exec_top_command(dh: Arc<Mutex<DataHolder>>) {
    let output = Command::new("top").arg("-b").arg("-n").arg("1").arg("-w").arg("512").output().unwrap();
    if output.status.success() {
        let lines = output.stdout.split(|&el| el == 10);
        let mut tasks = FuturesUnordered::new();
        for line in lines {
            let task = handle_line(str::from_utf8(&line).unwrap(), dh.clone());
            tasks.push(task);
        }
        while let Some(result) = tasks.next().await {
            if let Err(emsg) = result {
                eprintln!("Line subcommand error: {}", emsg);
            }
        }
    } else {
        let stderr = match str::from_utf8(&output.stderr) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Unable to convert stderr to UTF-8 string: {}", e);
                return;
            },
        };
        eprintln!("Command error: {}", stderr);
    }
}

async fn handle_netstat_line(header: &str, line: &str, dh: Arc<Mutex<DataHolder>>) -> Result<(), String> {
    let line = line.trim();
    let numbers = line.chars().map(|v| {
        if !v.is_ascii_digit() {
            '_'
        } else {
            v
        }
    }).collect::<String>();
    let numbers = numbers.split('_').filter(|&v| v != "").collect::<Vec<&str>>();
    for nr in numbers {
        let desc = line.replace(nr, "");
        let nr = match nr.parse::<f64>() {
            Ok(n) => n,
            Err(e) => {
                return Err(format!("Unable to convert NETSTAT value to float: {}", e));
            },
        };
        let desc = desc.trim_matches(|c| c == ' ' || c == ':');
        let desc = desc.split(" ").filter(|&v| v != "").collect::<Vec<&str>>().join(" ");
        let mut labels : Vec<(&str, &str)> = vec![];
        labels.push(("category", header));
        labels.push(("desc", &desc));
        send_influx_value(dh.clone(), NETSTAT_INFO_ID, Some(labels), nr);
    }
    Ok(())
}

async fn exec_netstat_command(dh: Arc<Mutex<DataHolder>>) {
    let output = Command::new("netstat").arg("-s").output().unwrap();
    if output.status.success() {
        let lines = output.stdout.split(|&el| el == 10);
        let mut tasks = FuturesUnordered::new();
        let mut header_line = "";
        for line in lines {
            if line.len() < 1 {
                continue;
            }
            if line[0] != 32 && line[line.len()-1] == 58 {
                header_line = str::from_utf8(&line[..line.len()-1]).unwrap();
                continue;
            }
            let task = handle_netstat_line(header_line, str::from_utf8(&line).unwrap(), dh.clone());
            tasks.push(task);
        }
        while let Some(result) = tasks.next().await {
            if let Err(emsg) = result {
                eprintln!("Line subcommand error NETSTAT: {}", emsg);
            }
        }
    } else {
        let stderr = match str::from_utf8(&output.stderr) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Unable to convert NETSTAT stderr to UTF-8 string: {}", e);
                return;
            },
        };
        eprintln!("Command error: {}", stderr);
    }
}

fn main() {
    let thread_lock_a = Arc::new(Mutex::new(Arc::new(Mutex::new(DataHolder::new()))));
    let thread_lock_b = thread_lock_a.clone();

    task::spawn(async move {
        loop {
            {
                let lock = thread_lock_a.lock().await;
                {
                    let mut inner_lock = (*lock).lock().await;
                    (*inner_lock).clear_data();
                }
                let top_handle = exec_top_command((*lock).clone());
                let netstat_handle = exec_netstat_command((*lock).clone());
                join!(top_handle, netstat_handle);
            }
            sleep(Duration::from_secs(5));
        }
    });


    let server = tiny_http::Server::http("0.0.0.0:8787").unwrap();
    loop {
        let rq = server.recv();
        if let Ok(request) = rq {
            let resp : Response<Cursor<Vec<u8>>>;
            if request.url() == "/metrics" {
                let thread_lock_b = thread_lock_b.clone();
                resp = task::block_on(async move {
                    let lock = thread_lock_b.lock().await;
                    let inner_lock = (*lock).lock().await;
                    let items = (*inner_lock).get_data();
                    let mut ans = items.iter().fold(String::new(), move |acc, (k,v)| acc + &k + " " + &v.to_string() + "\n");
                    ans.pop();
                    Response::from_string(ans).with_status_code(200)
                });
            } else {
                resp = Response::from_string("running ...").with_status_code(200);
            }
            if let Err(e) = request.respond(resp) {
                eprintln!("Response error: {}", e)
            }
        } else if let Err(e) = rq {
            eprintln!("Request error: {}", e);
        }
    }
}
