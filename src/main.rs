#![feature(type_name_of_val)]


use std::time::{Duration, Instant, SystemTime};
use zebra_chain::{parameters::Network};
use std::sync::Mutex;
use std::{
    sync::Arc,
};



use tower::Service;

use zebra_network::{connect_isolated_tcp_direct, Request};

use std::thread::sleep;
use std::net::{SocketAddr, ToSocketAddrs};


use tonic::{transport::Server, Request as TonicRequest, Response as TonicResponse, Status};

use seeder_proto::seeder_server::{Seeder, SeederServer};
use seeder_proto::{SeedRequest, SeedReply};

pub mod seeder_proto {
    tonic::include_proto!("seeder"); // The string specified here must match the proto package name
}

#[derive(Debug)]
pub struct SeedContext {
    peer_tracker_shared: Arc<Mutex<Vec<PeerStats>>>
}

#[tonic::async_trait]
impl Seeder for SeedContext {
    async fn seed(
        &self,
        request: TonicRequest<SeedRequest>, // Accept request of type SeedRequest
    ) -> Result<TonicResponse<SeedReply>, Status> { // Return an instance of type SeedReply
        println!("Got a request: {:?}", request);
        let peer_tracker_shared = self.peer_tracker_shared.lock().unwrap();
        let mut peer_strings = Vec::new();
        for peer in peer_tracker_shared.iter() {
            peer_strings.push(format!("{:?}",peer.address))
        }
        let reply = seeder_proto::SeedReply {
            ip: peer_strings
        };

        Ok(TonicResponse::new(reply)) // Send back our formatted greeting
    }
}




#[derive(Debug)]
enum PollResult {
    ConnectionFail,
    RequestFail,
    PollOK
}

async fn test_a_server(peer_addr: SocketAddr) -> PollResult
{
    println!("Starting new connection: peer addr is {:?}", peer_addr);
    let the_connection = connect_isolated_tcp_direct(Network::Mainnet, peer_addr, String::from("/Seeder-and-feeder:0.0.0-alpha0/"));
    let x = the_connection.await;

    match x {
        Ok(mut z) => {
            let resp = z.call(Request::Peers).await;
            match resp {
                Ok(res) => {
                println!("peers response: {}", res);
                return PollResult::PollOK;
            }
                Err(error) => {
                println!("peer error: {}", error);
                return PollResult::RequestFail;
            }
            }
        }



        Err(error) => {
            println!("Connection failed: {:?}", error);
            return PollResult::ConnectionFail;
        }
    };
}

#[derive(Debug, Clone, Copy, Default)]
struct EWMAState {
    scale:       Duration,
    weight:      f64,
    count:       f64,
    reliability: f64,
}

#[derive(Debug, Clone, Copy)]
struct PeerStats {
    address: SocketAddr,
    total_attempts: i32,
    total_successes: i32,
    ewma_pack: EWMAPack,
    last_polled: Instant,
    last_polled_absolute: SystemTime
}

#[derive(Debug, Clone, Copy)]
struct EWMAPack{
    stat_2_hours: EWMAState,
    stat_8_hours: EWMAState,
    stat_1day: EWMAState,
    stat_1week: EWMAState,
    stat_1month: EWMAState
}

impl Default for EWMAPack {
    fn default() -> Self { EWMAPack {
        stat_2_hours: EWMAState {scale: Duration::new(3600*2,0), ..Default::default()},
        stat_8_hours: EWMAState {scale: Duration::new(3600*8,0), ..Default::default()},
        stat_1day: EWMAState {scale: Duration::new(3600*24,0), ..Default::default()},
        stat_1week: EWMAState {scale: Duration::new(3600*24*7,0), ..Default::default()},
        stat_1month: EWMAState {scale: Duration::new(3600*24*30,0), ..Default::default()}
    }
    }
}
fn update_ewma(prev: &mut EWMAState, sample_age: Duration, sample: bool) {
    let weight_factor = (-sample_age.as_secs_f64()/prev.scale.as_secs_f64()).exp();

    let sample_value:f64 = sample as i32 as f64;
    println!("sample_value is: {}, weight_factor is {}", sample_value, weight_factor);
    prev.reliability = prev.reliability * weight_factor + sample_value * (1.0-weight_factor);

    // I don't understand what this and the following line do
    prev.count = prev.count * weight_factor + 1.0;

    prev.weight = prev.weight * weight_factor + (1.0-weight_factor);
}

fn update_ewma_pack(prev: &mut EWMAPack, last_polled: Instant, sample: bool) {
    let current = Instant::now();
    let sample_age = current.duration_since(last_polled);
    update_ewma(&mut prev.stat_2_hours, sample_age, sample);
    update_ewma(&mut prev.stat_8_hours, sample_age, sample);
    update_ewma(&mut prev.stat_1day, sample_age, sample);
    update_ewma(&mut prev.stat_1week, sample_age, sample);
    update_ewma(&mut prev.stat_1month, sample_age, sample);
}


fn is_good(peer: PeerStats) -> bool {
/*
    if (ip.GetPort() != GetDefaultPort()) return false;
    if (!(services & NODE_NETWORK)) return false;
    if (!ip.IsRoutable()) return false;
    if (clientVersion && clientVersion < REQUIRE_VERSION) return false;
    if (blocks && blocks < GetRequireHeight()) return false;
*/
    let ewmas = peer.ewma_pack;
    if peer.total_attempts <= 3 && peer.total_successes * 2 >= peer.total_attempts {return true};

    if ewmas.stat_2_hours.reliability > 0.85 && ewmas.stat_2_hours.count > 2.0  {return true};
    if ewmas.stat_8_hours.reliability > 0.70 && ewmas.stat_8_hours.count > 4.0  {return true};
    if ewmas.stat_1day.reliability > 0.55 && ewmas.stat_1day.count > 8.0  {return true};
    if ewmas.stat_1week.reliability > 0.45 && ewmas.stat_1week.count > 16.0 {return true};
    if ewmas.stat_1month.reliability > 0.35 && ewmas.stat_1month.count > 32.0 {return true};

    return false;
}

fn get_ban_time(peer: PeerStats) -> Option<Duration> {
    if is_good(peer) {return None}
    // if (clientVersion && clientVersion < 31900) { return 604800; }
    let ewmas = peer.ewma_pack;

    if ewmas.stat_1month.reliability - ewmas.stat_1month.weight + 1.0 < 0.15 && ewmas.stat_1month.count > 32.0 { return Some(Duration::from_secs(30*86400)); }
    if ewmas.stat_1week.reliability - ewmas.stat_1week.weight + 1.0 < 0.10 && ewmas.stat_1week.count > 16.0 { return Some(Duration::from_secs(7*86400));  }
    if ewmas.stat_1day.reliability - ewmas.stat_1day.weight + 1.0 < 0.05 && ewmas.stat_1day.count > 8.0  { return Some(Duration::from_secs(1*86400));  }
    return None;
}

fn get_ignore_time(peer: PeerStats) -> Option<Duration> {
    if is_good(peer) {return None}
    let ewmas = peer.ewma_pack;

    if ewmas.stat_1month.reliability - ewmas.stat_1month.weight + 1.0 < 0.20 && ewmas.stat_1month.count > 2.0  { return Some(Duration::from_secs(10*86400)); }
    if ewmas.stat_1week.reliability - ewmas.stat_1week.weight + 1.0 < 0.16 && ewmas.stat_1week.count > 2.0  { return Some(Duration::from_secs(3*86400));  }
    if ewmas.stat_1day.reliability - ewmas.stat_1day.weight + 1.0 < 0.12 && ewmas.stat_1day.count > 2.0  { return Some(Duration::from_secs(8*3600));   }
    if ewmas.stat_8_hours.reliability - ewmas.stat_8_hours.weight + 1.0 < 0.08 && ewmas.stat_8_hours.count > 2.0  { return Some(Duration::from_secs(2*3600));   }
    return None;
}



#[tokio::main]
async fn main()
{
    let addr = "127.0.0.1:50051".parse().unwrap();
    let peer_tracker_shared = Arc::new(Mutex::new(Vec::new()));

    let seedfeed = SeedContext {peer_tracker_shared: peer_tracker_shared.clone()};

    let seeder_service = Server::builder()
        .add_service(SeederServer::new(seedfeed))
        .serve(addr);

    tokio::spawn(seeder_service);

//    let peer_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(34, 127, 5, 144)), 8233);
    //let peer_addr = "157.245.172.190:8233".to_socket_addrs().unwrap().next().unwrap();
    let peer_addrs = ["34.127.5.144:8233", "157.245.172.190:8233"];
    let mut internal_peer_tracker = Vec::new();


    
    for peer in peer_addrs {
        let i = PeerStats {address: peer.to_socket_addrs().unwrap().next().unwrap(),
            total_attempts: 0,
            total_successes: 0,
            ewma_pack: EWMAPack::default(),
            last_polled: Instant::now(),
            last_polled_absolute: SystemTime::now()};
        internal_peer_tracker.push(i);
    }

    loop {
        for peer in internal_peer_tracker.iter_mut() {
            let poll_time = Instant::now();
            let poll_res = test_a_server(peer.address).await;
            println!("result = {:?}", poll_res);
            peer.total_attempts += 1;
            match poll_res {
                PollResult::PollOK => {
                    peer.total_successes += 1;
                    update_ewma_pack(&mut peer.ewma_pack, peer.last_polled, true);
                }
                _ => {
                    update_ewma_pack(&mut peer.ewma_pack, peer.last_polled, false);
                }
            }
            peer.last_polled_absolute = SystemTime::now();

            peer.last_polled = poll_time;
            println!("updated peer stats = {:?}", peer);
        }
        let mut unlocked = peer_tracker_shared.lock().unwrap();
        *unlocked = internal_peer_tracker.clone();
        std::mem::drop(unlocked);

        sleep(Duration::new(4,0));
    }
    // .to_socket_addrs().unwrap().next().unwrap();
    // loop {
    //     //let peer_addr = SocketAddr::new(proband_ip, proband_port);
    //     test_a_server(peer_addr).await;
    //     sleep(Duration::new(5, 0));
    // }


    
}