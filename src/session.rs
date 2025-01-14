//! This file is adapted from the actix-web chat websocket example

use std::time::{Duration, Instant};

use actix::prelude::*;
use actix_web_actors::ws;

use serde_json::Value;
use uuid::Uuid;

use crate::hub;
use crate::logic::{PlayerAction, PLAYER_TIMEOUT};
use crate::messages;

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(20);

pub fn get_help_message() -> Vec<String> {
    vec!["/small_blind AMOUNT".to_string(),
	 "/big_blind AMOUNT".to_string(),
	 "/starting_stack AMOUNT".to_string(),
	 "/set_password PASSWORD".to_string(),
	 "/show_password".to_string(),	 
	 "/add_bot".to_string(),
	 "/remove_bot".to_string(),
	 "/restart".to_string()	 
    ]
}

#[derive(Debug)]
pub struct WsPlayerSession {
    /// unique session id
    pub id: Uuid,

    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    pub client_hb: Instant,

    // we also keep track of how long since they did a "real" command
    // if the player is inactive for too long, we stop the session to clear resources
    // Note: the hub also checks for a command heart beat to clear the player config itself from the lobby
    pub command_hb: Instant,
    
    /// Table hub address
    pub hub_addr: Addr<hub::TableHub>,
}

impl WsPlayerSession {
    pub fn new(hub_addr: Addr<hub::TableHub>) -> Self {
        let id = Uuid::new_v4();
	println!("brand new uuid = {id}");
        Self {
            id,
            client_hb: Instant::now(),
            command_hb: Instant::now(),	    
            hub_addr,
        }
    }

    /// if the client wants to reconnect with an existing uuid
    pub fn from_existing(uuid: Uuid, hub_addr: Addr<hub::TableHub>) -> Self {
        Self {
            id: uuid,
            client_hb: Instant::now(),
            command_hb: Instant::now(),	    
            hub_addr,
        }
    }
    
    /// helper method that sends ping to client every 5 seconds (HEARTBEAT_INTERVAL).
    ///
    /// also this method checks heartbeats from client
    /// I believe this latter check is usually redundant, since if the client closers their
    /// browser, for example, it will trigger a stopping() call. I guess if the client can't even
    /// respond at all, this heartbeat could come in handy?
    fn hb(&self, ctx: &mut ws::WebsocketContext<Self>) {
        ctx.run_interval(HEARTBEAT_INTERVAL, |act, ctx| {
            // check client heartbeats
	    let command_gap = Instant::now().duration_since(act.command_hb);	    
            if command_gap > PLAYER_TIMEOUT + Duration::from_secs(30) {
                // command heartbeat timed out
		// Note: we wait a bit longer than the PLAYER_TIMEOUT, so that we might first receive
		// the message from the hub that we timed out
                println!("Session PLAYER heartbeat failed, disconnecting!");

                // stop actor
                ctx.stop();

                // don't try to send a ping
                return;
            }

	    let client_gap = Instant::now().duration_since(act.client_hb);	    
            if client_gap > CLIENT_TIMEOUT {
                // client heartbeat timed out
                println!("Websocket Client heartbeat failed, disconnecting!");
		// Note: here we do NOT tell the hub that we want to leave the table.
		// This allows for the client to rejoin with the same UUID and a new session
		// (Up to the PLAYER_TIMEOUT)
		
                // stop actor
                ctx.stop();

                // don't try to send a ping
                return;
            }

            ctx.ping(b"");
        });
    }
}

impl Actor for WsPlayerSession {
    type Context = ws::WebsocketContext<Self>;

    /// Method is called on actor start.
    /// We register ws session with the hub
    fn started(&mut self, ctx: &mut Self::Context) {
        // we'll start heartbeat process on session start.
        self.hb(ctx);

        // register self in hub. `AsyncContext::wait` register
        // future within context, but context waits until this future resolves
        // before processing any other events.
        // HttpContext::state() is instance of WsPlayerSessionState, state is shared
        // across all routes within application
        let addr = ctx.address();
        self.hub_addr
            .send(messages::Connect {
		id: self.id,
                addr: addr.recipient(),
            })
            .into_actor(self)
            .then(|res, act, ctx| {
                match res {
                    Ok(res) => {
			act.id = res;
		    },
                    // something is wrong with the hub
                    _ => ctx.stop(),
                }
                fut::ready(())
            })
            .wait(ctx);
    }

    fn stopping(&mut self, _: &mut Self::Context) -> Running {
        Running::Stop
    }
}

/// Handle messages from hub, we simply send it to peer websocket
impl Handler<messages::WsMessage> for WsPlayerSession {
    type Result = ();

    fn handle(&mut self, msg: messages::WsMessage, ctx: &mut Self::Context) {
        ctx.text(msg.0);
    }
}

/// WebSocket message handler
impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for WsPlayerSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        let msg = match msg {
            Err(_) => {
                ctx.stop();
                return;
            }
            Ok(msg) => msg,
        };

        log::debug!("WEBSOCKET MESSAGE: {msg:?}");
        match msg {
            ws::Message::Ping(msg) => {
                self.client_hb = Instant::now();
                ctx.pong(&msg);
            }
            ws::Message::Pong(_) => {
                self.client_hb = Instant::now();
            }
            ws::Message::Text(text) => {
                let m = text.trim();

                if let Ok(object) = serde_json::from_str(m) {
                    self.command_hb = Instant::now(); // we got a command, so set the heartbeat
                    println!("parsed: {}", object);
                    self.handle_client_command(object, m, ctx);
                } else {
                    println!("message unable to parse as json: {}", m);
                };
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

impl WsPlayerSession {
    fn handle_client_command(
        &mut self,
        object: Value,
	m: &str, // the original string in case we want to use it to parse
        ctx: &mut <WsPlayerSession as Actor>::Context,
    ) {
        println!("Entered handle_client_command {:?}", object);
        let msg_type_opt = object.get("msg_type");
        if msg_type_opt.is_none() {
            println!("missing message type!");
            return;
        }
        let msg_type = msg_type_opt.unwrap();
        match msg_type {
            Value::String(type_str) => match type_str.as_str() {
                "player_action" => {
                    self.handle_player_action(object, ctx);
                }
                "list" => {
                    self.handle_list_tables(ctx);
                }
                "join" => {
                    self.handle_join_table(object, ctx);
                }
                "create" => {
                    self.handle_create_table(m, ctx);
                }
                "admin_command" => {
                    self.handle_admin_command(object, ctx);
                }
                "leave" => {
                    self.hub_addr.do_send(messages::MetaActionMessage {
                        id: self.id,
                        meta_action: messages::MetaAction::Leave(self.id),
                    });
                }
                "imback" => {
                    self.hub_addr.do_send(messages::MetaActionMessage {
                        id: self.id,
                        meta_action: messages::MetaAction::ImBack(self.id),
                    });
                }
                "sitout" => {
		    // we actually send a meta action and a player action.
		    // Depending where we are in the game street loop,
		    // sending both guarantees it will respond quickly
                    self.hub_addr.do_send(messages::PlayerActionMessage {
                        id: self.id,
                        player_action: PlayerAction::SitOut,
                    });		    		    
                    self.hub_addr.do_send(messages::MetaActionMessage {
                        id: self.id,
                        meta_action: messages::MetaAction::SitOut(self.id),
                    });
                }
                "name" => {
                    self.handle_player_name(object, ctx);
                }
                "chat" => {
                    self.handle_chat(object, ctx);
                }
		"help" => {
                    let message = json::object! {
			msg_type: "help_message".to_owned(),
			commands: get_help_message(),
                    };
                    ctx.text(message.dump());
		}
                _ => ctx.text(format!("!!! unknown command: {:?}", object)),
            },
            _ => ctx.text(format!("!!! improper msg_type in: {:?}", object)),
        }
    }

    fn handle_create_table(&self, msg: &str, ctx: &mut <WsPlayerSession as Actor>::Context) {
        self.hub_addr
            .send(messages::Create {
                id: self.id,
                create_msg: msg.into(),
            })
            .into_actor(self)
            .then(|res, _, ctx| {
                match res {
                    Ok(create_table_result) => match create_table_result {
                        Ok(table_name) => {
                            println!("created table = {}", table_name);
                            let message = json::object! {
                                msg_type: "created_table".to_owned(),
                                table_name: table_name,
                            };
                            ctx.text(message.dump());
                        }
                        Err(e) => {
                            println!("{}", e);
                            let message = json::object! {
                                            msg_type: "error".to_owned(),
                            error: "unable_to_create".to_owned(),
                                            reason: e.to_string(),
                                        };
                            ctx.text(message.dump());
                        }
                    },
                    _ => println!("MailBox error"),
                }
                fut::ready(())
            })
            .wait(ctx)
        // .wait(ctx) pauses all events in context,
        // so actor wont receive any new messages until it get list
        // of tables back
    }
    
    fn handle_list_tables(&self, ctx: &mut <WsPlayerSession as Actor>::Context) {
        // Send ListTables message to the hub and wait for response
        println!("List tables");
        let addr = ctx.address();	
        self.hub_addr
            .send(messages::ListTables(addr.recipient()))
            .into_actor(self)
            .then(|res, _, ctx| {
                match res {
                    Ok(tables) => {
                        let message = json::object! {
                            msg_type: "tables_list".to_owned(),
                            tables: tables,
                        };
                        ctx.text(message.dump());
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

    fn handle_join_table(&self, object: Value, ctx: &mut <WsPlayerSession as Actor>::Context) {
        if let (Some(Value::String(table_name)), Some(password)) =
            (object.get("table_name"), object.get("password"))
        {
            let table_name = table_name.to_string();
            let password = if let Some(password) = password.as_str()  {
                Some(password.to_owned())
            } else {
                None
            };
            self.hub_addr.do_send(messages::Join {
                id: self.id,
                table_name,
                password,
            });
        } else {
            println!("missing table name or password!");
            ctx.text("!!! table_name and password (possibly null) are required");
        }
    }

    fn handle_player_action(&self, object: Value, ctx: &mut <WsPlayerSession as Actor>::Context) {
        if let Some(Value::String(player_action)) = object.get("action") {
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
                    if let Some(Value::String(amount)) = object.get("amount") {
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
                    ctx.text(format!(
                        "invalid action set for type:player_action: {:?}",
                        other
                    ));
                }
            }
        } else {
            ctx.text("!!! action is required");
        }
    }

    fn handle_player_name(&self, object: Value, ctx: &mut <WsPlayerSession as Actor>::Context) {
        if let Some(Value::String(name)) = object.get("player_name") {
            println!("{}", name);
            self.hub_addr.do_send(messages::PlayerName {
                id: self.id,
                name: name.to_string(),
            });
        } else {
            ctx.text("!!! player_name is required");
        }
    }

    fn handle_chat(&self, object: Value, ctx: &mut <WsPlayerSession as Actor>::Context) {
        if let Some(Value::String(text)) = object.get("text") {
            let text = text.to_string();
            self.hub_addr.do_send(messages::MetaActionMessage {
                id: self.id,
                meta_action: messages::MetaAction::Chat(self.id, text),
            })
        } else {
            println!("missing chat_message!");
            ctx.text("!!! chat_message is required");
        }
    }

    // e.g. {"msg_type": "admin_command", "admin_command": "big_blind", "big_blind": 24}
    fn handle_admin_command(&self, object: Value, ctx: &mut <WsPlayerSession as Actor>::Context) {
        if let Some(Value::String(admin_command)) = object.get("admin_command") {
            let invalid_json =  match admin_command.as_str() {
                "small_blind" => {
		    if let Some(Value::String(amount)) = object.get("small_blind") {
			if let Ok(amount) = amount.to_string().parse::<u32>() {
			    self.hub_addr.do_send(messages::MetaActionMessage {
				id: self.id,
				meta_action: messages::MetaAction::Admin(
				    self.id,				
				    messages::AdminCommand::SmallBlind(amount),
				)
			    });
			    false
			} else {
			    true
			}
		    } else {
			// invalid_json
			true
		    }
                }
                "big_blind" => {
		    if let Some(Value::String(amount)) = object.get("big_blind") {
			if let Ok(amount) = amount.to_string().parse::<u32>() {			
			    self.hub_addr.do_send(messages::MetaActionMessage {
				id: self.id,
				meta_action: messages::MetaAction::Admin(
				    self.id,				
				    messages::AdminCommand::BigBlind(amount),
				)
			    });
			    false			    
			} else {
			    true
			}
		    } else {
			// invalid_json			
			true
		    }
                }
                "starting_stack" => {
		    if let Some(Value::String(amount)) = object.get("starting_stack") {
			if let Ok(amount) = amount.to_string().parse::<u32>() {	
			    self.hub_addr.do_send(messages::MetaActionMessage {
				id: self.id,
				meta_action: messages::MetaAction::Admin(
				    self.id,				
				    messages::AdminCommand::BuyIn(amount),
				)
			    });
			    false
			} else {
			    true
			}
		    } else {
			// invalid json
			true
		    }		    
                }		
                "set_password" => {
		    if let Some(Value::String(amount)) = object.get("set_password") {
			let amount = amount.to_string();
			self.hub_addr.do_send(messages::MetaActionMessage {
			    id: self.id,
			    meta_action: messages::MetaAction::Admin(
				self.id,				
				messages::AdminCommand::SetPassword(amount),
			    )
			});
			false
		    } else {
			// invalid json
			true
		    }
                }
                "show_password" => {
		    self.hub_addr.do_send(messages::MetaActionMessage {
			id: self.id,
			meta_action: messages::MetaAction::Admin(
			    self.id,				
			    messages::AdminCommand::ShowPassword),
                    });
		    false
		}		
                "add_bot" => {
		    self.hub_addr.do_send(messages::MetaActionMessage {
			id: self.id,
			meta_action: messages::MetaAction::Admin(
			    self.id,				
			    messages::AdminCommand::AddBot),
                    });
		    false
		}
                "remove_bot" => {
		    self.hub_addr.do_send(messages::MetaActionMessage {
			id: self.id,
			meta_action: messages::MetaAction::Admin(
			    self.id,				
			    messages::AdminCommand::RemoveBot),
                    });
		    false
                }
                "restart" => {
		    self.hub_addr.do_send(messages::MetaActionMessage {
			id: self.id,
			meta_action: messages::MetaAction::Admin(
			    self.id,				
			    messages::AdminCommand::Restart),
                    });
		    false
                }
                _ => {
		    // invalid command
		    true 
                }
            };
	    if invalid_json {
                let message = json::object! {
                    msg_type: "error".to_owned(),
		    error: "invalid_admin_command".to_owned(),
                    reason: "this admin_command was invalid.".to_owned(),
                };
                ctx.text(message.dump());
		
	    }
	}
    }
    

    
}
