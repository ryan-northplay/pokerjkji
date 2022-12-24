//! This file is adapted from the actix-web chat websocket example

use std::time::{Duration, Instant};

use actix::prelude::*;
use actix_web_actors::ws;

use uuid::Uuid;
use serde_json::Value;

use crate::hub;
use crate::logic::player::PlayerAction;
use crate::messages;

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct WsGameSession {
    /// unique session id
    pub id: Uuid,

    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    pub hb: Instant,

    /// joined table (if at one)
    // ADAM: rfemoving this. should the session need to know anything excect how to contact the gamehub
    //pub table: Option<String>,

    /// user name
    //pub name: Option<String>,

    /// Game hub address
    pub hub_addr: Addr<hub::GameHub>,
}

impl WsGameSession {
    pub fn new(hub_addr: Addr<hub::GameHub>) -> Self {
        Self {
            id: Uuid::new_v4(),
            hb: Instant::now(),
            //table: None,
            //name: None,
            hub_addr,
        }
    }

    /// helper method that sends ping to client every 5 seconds (HEARTBEAT_INTERVAL).
    ///
    /// also this method checks heartbeats from client
    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // check client heartbeats
            if Instant::now().duration_since(act.hb) > CLIENT_TIMEOUT {
                // heartbeat timed out
                println!("Websocket Client heartbeat failed, disconnecting!");

		// notify game server. A Leave is the same thing for the game
		act.hub_addr.do_send(messages::MetaActionMessage {
		    id: act.id,
		    meta_action: messages::MetaAction::Leave(act.id),
		});
		
                // stop actor
                ctx.stop();

                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }
}

impl Actor for WsGameSession {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start.
    /// We register ws session with GameServer
    fn started(&mut self, ctx: &mut Self::Context) {
        // we'll start heartbeat process on session start.
        self.hb(ctx);

        // register self in game server. `AsyncContext::wait` register
        // future within context, but context waits until this future resolves
        // before processing any other events.
        // HttpContext::state() is instance of WsGameSessionState, state is shared
        // across all routes within application
        let addr = ctx.address();
        self.hub_addr
            .send(messages::Connect {
                addr: addr.recipient(),
            })
            .into_actor(self)
            .then(|res, act, ctx| {
                match res {
                    Ok(res) => act.id = res,
                    // something is wrong with game server
                    _ => ctx.stop(),
                }
                fut::ready(())
            })
            .wait(ctx);
    }

    fn stopping(&mut self, _: &mut Self::Context) -> Running {
        // notify game server. A Leave is the same thing for the game
        self.hub_addr.do_send(messages::MetaActionMessage {
            id: self.id,
            meta_action: messages::MetaAction::Leave(self.id),
        });
        Running::Stop
    }
}

/// Handle messages from game server, we simply send it to peer websocket
impl Handler<messages::WsMessage> for WsGameSession {
    type Result = ();

    fn handle(&mut self, msg: messages::WsMessage, ctx: &mut Self::Context) {
        ctx.text(msg.0);
    }
}

/// WebSocket message handler
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsGameSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        let msg = match msg {
            Err(_) => {
                ctx.stop();
                return;
            }
            Ok(msg) => msg,
        };

        log::debug!("WEBSOCKET MESSAGE: {msg:?}");
        //println!("WEBSOCKET MESSAGE: {:?}", msg);	
        match msg {
            ws::Message::Ping(msg) => {
                self.hb = Instant::now();
                ctx.pong(&msg);
            }
            ws::Message::Pong(_) => {
                self.hb = Instant::now();
            }
            ws::Message::Text(text) => {
                let m = text.trim();
		if let Ok(object) = serde_json::from_str(m) {
		    // we were able to parse the given text as a json
                    self.handle_client_command(object, ctx);		    
		} else {
		    println!("message unable to parse as json: {}", m);
		}		
	    }
            ws::Message::Binary(_) => println!("Unexpected binary"),
            ws::Message::Close(reason) => {
                ctx.close(reason);
                ctx.stop();
            }
            ws::Message::Continuation(_) => {
                ctx.stop();
            }
            ws::Message::Nop => (),
        }
    }
}

impl WsGameSession {
    fn handle_client_command(
        &mut self,
        object: Value,
        ctx: &mut <WsGameSession as Actor>::Context,
    ) {
	println!("Entered handle_client_command {:?}", object);
	let msg_type_opt = object.get("msg_type");
	if msg_type_opt.is_none() {
	    println!("missing message type!");
	    return;
	}
	let msg_type = msg_type_opt.unwrap();
        match msg_type {
	    Value::String(type_str) => {
		match type_str.as_str() {
		    "player_action" => {
			self.handle_player_action(object, ctx);
		    }
		    "list" => {
			self.handle_list_tables(object, ctx);			
		    }
		    "join" => {
			self.handle_join_table(object, ctx);
		    }
		    "create" => {
			self.handle_create_table(object, ctx);
		    }		    
		    "leave" => {
			self.hub_addr.do_send(messages::MetaActionMessage {
			    id: self.id,
			    meta_action: messages::MetaAction::Leave(self.id),
			});
		    }	    
		    "sitout" => {
			self.hub_addr.do_send(messages::MetaActionMessage {
			    id: self.id,
			    meta_action: messages::MetaAction::SitOut(self.id),
			});
		    }	    
		    "imback" => {
			self.hub_addr.do_send(messages::MetaActionMessage {
			    id: self.id,
			    meta_action: messages::MetaAction::ImBack(self.id),
			});
		    }	    
		    "name" => {
			self.handle_player_name(object, ctx);
		    }
		    "chat" => {
			self.handle_chat(object, ctx);
		    }
		    _ => ctx.text(format!("!!! unknown command: {:?}", object)),
		}
	    }
	    _ => ctx.text(format!("!!! improper msg_type in: {:?}", object)),
	}
    }

    fn handle_create_table(&self, object: Value, ctx: &mut <WsGameSession as Actor>::Context) {
	self.hub_addr
	    .send(messages::Create {
		id: self.id,
	    })
	    .into_actor(self)
	    .then(|res, _, ctx| {
		match res {
		    Ok(create_game_result) => {
			match create_game_result {
			    Ok(room_name) => {
				println!("created game = {}", room_name);
				ctx.text(format!("created game = {}", room_name));
			    },
			    Err(e) => {
				println!("{}", e);
				ctx.text(format!("{}", e));
			    },
			}
		    }
		    _ => println!("MailBox error"),
		}
		fut::ready(())
	    })
	    .wait(ctx)
	// .wait(ctx) pauses all events in context,
	// so actor wont receive any new messages until it get list
	// of tables back					    
	
    }
    
    fn handle_list_tables(&self, object: Value, ctx: &mut <WsGameSession as Actor>::Context) {
	// Send ListTables message to game server and wait for
	// response
	println!("List tables");
	self.hub_addr
	    .send(messages::ListTables)
	    .into_actor(self)
	    .then(|res, _, ctx| {
		match res {
		    Ok(tables) => {
			ctx.text(format!("there are {:?} tables:", tables.len()));
			for table in tables {
			    ctx.text(table);
			}
		    }
		    _ => println!("Something is wrong"),
		}
		fut::ready(())
	    })
	    .wait(ctx)
	// .wait(ctx) pauses all events in context,
	// so actor wont receive any new messages until it get list
	// of tables back
    }
    
    fn handle_join_table(&self, object: Value, ctx: &mut <WsGameSession as Actor>::Context) {
	if let Some(table_name) = object.get("table_name") {
	    let table_name = table_name.to_string();
	    self.hub_addr.do_send(messages::Join {
		id: self.id,
		table_name,
	    });
	} else {
	    println!("missing table name!");
	    ctx.text("!!! table_name is required");	    
	    return;
	}
    }

    fn handle_player_action(&self, object: Value, ctx: &mut <WsGameSession as Actor>::Context) {
	if let Some(player_action) = object.get("action") {
	    let player_action = player_action.to_string();
	    match player_action.as_str() {
		"check" => {
		    self.hub_addr.do_send(messages::PlayerActionMessage {
			id: self.id,
			player_action: PlayerAction::Check,
		    });
		}
		"fold" => {
		    self.hub_addr.do_send(messages::PlayerActionMessage {
			id: self.id,
			player_action: PlayerAction::Fold,
		    });
		}
		"call" => {
		    self.hub_addr.do_send(messages::PlayerActionMessage {
			id: self.id,
			player_action: PlayerAction::Call,
		    });
		}
		"bet" => {
		    if let Some(amount) = object.get("amount") {
			let amount = amount.to_string();
			self.hub_addr.do_send(messages::PlayerActionMessage {
			    id: self.id,
			    player_action: PlayerAction::Bet(amount.parse::<u32>().unwrap()),
			});
			//ctx.text(format!("placing bet of: {:?}", v[1]));
		    } else {
			ctx.text("!!!You much specify how much to bet!");		    
		    }
		}
		other => {
		    ctx.text(format!("invalid action set for type:player_action: {:?}", other));
		}
	    }
	} else {
	    ctx.text("!!! action is required");
	}	    
    }

    fn handle_player_name(&self, object: Value, ctx: &mut <WsGameSession as Actor>::Context) {
	if let Some(player_name) = object.get("player_name") {
	    let name = player_name.to_string();
	    self.hub_addr
		.do_send(messages::PlayerName { id: self.id, name });
	} else {
	    ctx.text("!!! player_name is required");
	}
    }

    fn handle_chat(&self, object: Value, ctx: &mut <WsGameSession as Actor>::Context) {
	if let Some(chat_message) = object.get("chat_message") {
	    let chat_message = chat_message.to_string();
	    self.hub_addr.do_send(
		messages::MetaActionMessage {
		    id: self.id,
		    meta_action: messages::MetaAction::Chat(self.id, chat_message)
		}
	    )
	} else {
	    println!("missing chat_message!");
	    ctx.text("!!! chat_message is required");	    
	}
    }
}
