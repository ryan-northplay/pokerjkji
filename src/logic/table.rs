use actix::Addr;
use json::object;
use rand::Rng;
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::convert::TryInto;

use super::card::Card;
use super::deck::{Deck, StandardDeck};
use super::game_hand::{GameHand, Street};

use super::player::{Player, PlayerAction, PlayerConfig};
use crate::hub::TableHub;

use crate::messages::{AdminCommand, GameOver, JoinTableError, MetaAction, Returned, ReturnedReason, WsMessage};

use std::{cmp, sync::Arc, thread, time};

use uuid::Uuid;

// any game that runs for too long without a human will end, rather than looping indefinitely
const NON_HUMAN_HANDS_LIMIT: u32 = 3;

#[derive(Debug)]
pub struct Table {
    hub_addr: Option<Addr<TableHub>>, // needs to be able to communicate back to the hub sometimes
    pub name: String,
    deck: Box<dyn Deck>,
    players: [Option<Player>; 9], // 9 spots where players can sit
    player_ids_to_configs: HashMap<Uuid, PlayerConfig>,
    max_players: u8, // how many will we let in the game
    small_blind: u32,
    big_blind: u32,
    buy_in: u32,
    password: Option<String>,
    admin_id: Uuid,
    button_idx: usize, // index of the player with the button
    hand_num: u32, // keeps track of the current hand number
}

/// useful for unit tests, for example
impl Default for Table {
    fn default() -> Self {
        Self {
            hub_addr: None,
            name: "Table".to_owned(),
            deck: Box::new(StandardDeck::new()),
            players: Default::default(),
            player_ids_to_configs: HashMap::<Uuid, PlayerConfig>::new(),
            max_players: 9,
            small_blind: 4,
            big_blind: 8,
            buy_in: 1000,
            password: None,
	    admin_id: uuid::Uuid::new_v4(), // an arbitrary/random admin id
            button_idx: 0,
            hand_num: 1,
        }
    }
}

impl Table {
    /// the address of the TableHub is optional so that unit tests need not worry about it
    /// We can pass in a custom Deck object, but if not, we will just construct a StandardDeck
    pub fn new(
        hub_addr: Addr<TableHub>,
        name: String,
        deck_opt: Option<Box<dyn Deck>>,
        max_players: u8, // how many will we let in the game
        small_blind: u32,
        big_blind: u32,
        buy_in: u32,
        password: Option<String>,
	admin_id: Uuid,
    ) -> Self {
        let deck = if let Some(deck) = deck_opt {
	    deck
        } else {
            Box::new(StandardDeck::new())
        };
        Table {
            hub_addr: Some(hub_addr),
            name,
            deck,
            players: Default::default(),
            player_ids_to_configs: HashMap::<Uuid, PlayerConfig>::new(),
            max_players,
            small_blind,
            big_blind,
            buy_in,
            password,
	    admin_id,
            button_idx: 0,
            hand_num: 1,
        }
    }

    fn send_game_state(&self, gamehand_opt: Option<&GameHand>, game_suspended: bool) {
	let mut state_message = self.get_game_state_json(gamehand_opt, game_suspended);
	// go through each player, and update the personal information for their message
	// (i.e. hole cards, player index)
        for (i, player_spot) in self.players.iter().enumerate() {
            if let Some(player) = player_spot {
		state_message["your_index"] = i.into();
		if player.hole_cards.len() == 2 {
		    state_message["hole_cards"] = format!("{}{}",
							  player.hole_cards[0],
							  player.hole_cards[1])
			.into();
		} else {
		    state_message["hole_cards"] = json::Null;
		}
		
		PlayerConfig::send_specific_message(
		    &state_message.dump(),
		    player.id,
                    &self.player_ids_to_configs,
		);
		
            }
	}
    }
    
    /// returns the game state as a json-String, for sending to the front-end
    fn get_game_state_json(
	&self,
	gamehand_opt: Option<&GameHand>,
	game_suspended: bool,
    ) -> json::JsonValue {
        let mut state_message = object! {
            msg_type: "game_state".to_owned(),
            name: self.name.to_owned(),
            max_players: self.max_players,
            small_blind: self.small_blind,
            big_blind: self.big_blind,
            buy_in: self.buy_in,
            password: self.password.to_owned(),	    
            button_idx: self.button_idx,
            hand_num: self.hand_num,
	    game_suspended: game_suspended,
	};

	// add a list of player infos
	let mut player_infos = vec![];
        for (i, player_spot) in self.players.iter().enumerate() {
            // display the play positions for the front end to consume
            if let Some(player) = player_spot {
		if !self.player_ids_to_configs.contains_key(&player.id) {
		    // be safe, double check if config still exists
		    continue;
		}
                let config = self.player_ids_to_configs.get(&player.id).unwrap();		
                let mut player_info = object! {
                    index: i,
                };
                let name = config.name.as_ref().unwrap().clone();
                player_info["player_name"] = name.into();
                player_info["money"] = player.money.into();
                player_info["is_active"] = player.is_active.into();
		if player.is_sitting_out {
                    player_info["is_sitting_out"] = true.into();		    
		}
		if player.is_all_in() {
                    player_info["is_all_in"] = true.into();
		}
		if let Some(last_action) = player.last_action {
                    player_info["last_action"] = last_action.to_string().into();
		}
		if let Some(gamehand) = gamehand_opt {
		    for (street, contributions) in gamehand.street_contributions.iter() {
			match street {			    
			    Street::Preflop => {player_info["preflop_cont"] = contributions[i].into()}
			    Street::Flop => {player_info["flop_cont"] = contributions[i].into()}
			    Street::Turn => {player_info["turn_cont"] = contributions[i].into()}
			    Street::River => {player_info["river_cont"] = contributions[i].into()}
			    Street::ShowDown => (),
			}
		    }
		}
		player_infos.push(Some(player_info));
            } else {
		player_infos.push(None);
	    }
        }
	state_message["players"] = player_infos.into();

	if let Some(gamehand) = gamehand_opt {
	    state_message["street"] = gamehand.street.to_string().into();
	    state_message["current_bet"] = gamehand.current_bet.into();
	    
	    if let Some(flop) = &gamehand.flop {
		state_message["flop"] = format!(
		    "{}{}{}",
		    flop[0],
		    flop[1],
		    flop[2]
		)
		    .into();
	    }
	    if let Some(turn) = &gamehand.turn {
		state_message["turn"] = format!("{}", turn).into();
            }	
	    if let Some(river) = &gamehand.river {
            state_message["river"] = format!("{}", river).into();
            }
            state_message["pots"] = gamehand.pot_repr().into();

	    if let Some(index_to_act) = gamehand.index_to_act {
		state_message["index_to_act"] = index_to_act.into();
	    }
	}

	state_message
    }
	
    /// add a given playerconfig to an empty seat
    /// if the game requires a password, then a matching password must be provided for the user to be added
    /// TODO: eventually we wanmt the player to select an open seat I guess
    /// returns the index of the seat that they joined (if they were able to join)
    fn add_human(
        &mut self,
        player_config: PlayerConfig,
        password: Option<String>,
    ) -> Result<usize, JoinTableError> {
        if let Some(game_password) = &self.password {
            if let Some(given_password) = password {
                if game_password.ne(&given_password) {
                    // the provided password does not match the game password
                    return Err(JoinTableError::InvalidPassword);
                }
            } else {
                // we did not provide a password, but the game requires one
                return Err(JoinTableError::MissingPassword);
            }
        }
        let id = player_config.id; // copy so that we can send the messsage later
        let new_player = Player::new(id, true, self.buy_in);
        let result = self.add_player(player_config, new_player);
        result
    }

    pub fn add_bot(&mut self, name: String) -> Result<usize, JoinTableError> {
        let new_bot = Player::new_bot(self.buy_in);
        let new_config = PlayerConfig::new(new_bot.id, Some(name), None);
        self.add_player(new_config, new_bot)
    }

    fn add_player(
        &mut self,
        player_config: PlayerConfig,
        player: Player,
    ) -> Result<usize, JoinTableError> {
        // Kinda weird, but first check if the player is already at the table
        // Could happen if their Leave wasn't completed yet
        // TODO: verify this can actually happen. Unit testable even?
        for (i, player_spot) in self.players.iter_mut().enumerate() {
            if let Some(existing) = player_spot {
                if existing.id == player.id {
                    println!("the player was ALREADY at the table!");
                    self.player_ids_to_configs
                        .insert(player_config.id, player_config);
                    return Ok(i);
                }
            }
        }

        if self.players.iter().flatten().count() >= self.max_players.into() {
            // we already have as many as we can fit in the game
            return Err(JoinTableError::GameIsFull);
        }

        for (i, player_spot) in self.players.iter_mut().enumerate() {
            if player_spot.is_none() {
                *player_spot = Some(player);
                self.player_ids_to_configs
                    .insert(player_config.id, player_config);
                return Ok(i);
            }
        }
        // if we did not early return, then we must have been full
        Err(JoinTableError::GameIsFull)
    }

    /// if any of the player configs has not had a heart beat in a long time,
    /// we tell the hub (via a Returned message), and then removethe config from
    /// self.player_ids_to_configs
    fn handle_player_heart_beats(&mut self) {
	for (_uuid, config) in self.player_ids_to_configs.iter() {
	    if !config.has_active_heart_beat() {
                if let Some(hub_addr) = &self.hub_addr {
                    // tell the hub that we left
                    let cloned_config = config.clone(); // clone to send back to the hub
                    hub_addr.do_send(Returned {
                        config: cloned_config,
                        reason: ReturnedReason::HeartBeatFailed,
                    });
                }
	    }
	}
	// now remove the configs that failed the heart beat
	// They is probably a better way to code this method, but this works for now
        self.player_ids_to_configs.retain(|_uuid, config| {
            // if a player config has no active heartbeat (i.e. has not done anything in a long time)
            // then we remove their config               
            config.has_active_heart_beat()
        });
    }
    pub fn play(
        &mut self,
        incoming_actions: &Arc<Mutex<HashMap<Uuid, PlayerAction>>>,
        incoming_meta_actions: &Arc<Mutex<VecDeque<MetaAction>>>,
        hand_limit: Option<u32>, // how many hands total should be play? None == no limit
    ) {
        let mut non_human_hands = 0; // we only allow a certain number of hands without a human before ending
        loop {
	    let between_hands = true;

	    ////
	    self.handle_meta_actions(&incoming_meta_actions, between_hands, None);
	    self.handle_player_heart_beats();
            // check if any player left with a meta action or timed out due to heart beat.                 
            // if so, their config will be gone, so now remove the player struct as well.
            for player_spot in self.players.iter_mut() {
                if let Some(player) = player_spot {
                    if !self.player_ids_to_configs.contains_key(&player.id) {
                        println!("player is no longer in the config");
                        *player_spot = None;
			
                    }
		}
	    }
 	    
            if let Some(limit) = hand_limit {
                if self.hand_num > limit {
                    println!("hand limit has been reached");
                    break;
                }
            }
            println!(
                "\n\n\nPlaying hand {}, button_idx = {}",
                self.hand_num, self.button_idx
            );	    
            let num_human_players = self
                .players
                .iter()
                .flatten()
                .filter(|player| player.human_controlled)
                .count();
            println!("num human players == {:?}", num_human_players);
            println!("non human hands == {:?}", non_human_hands);
	    
            if num_human_players == 0 {
                non_human_hands += 1;
                println!("num human players == {:?}", num_human_players);
                println!("non human hands == {:?}", non_human_hands);
            }
            if non_human_hands > NON_HUMAN_HANDS_LIMIT {
                // the table ends no matter what if we haven't had a human after too many turns
                break;
            }

	    let was_played = self.play_one_hand(&incoming_actions, &incoming_meta_actions);
	    if was_played {
		// only increment the hand num and find a new button if we indeed played a hand.
		// if there are not enough players and/or active players, a hand is not dealt/played
		self.hand_num += 1;
		
		// attempt to set the next button
		self.button_idx = self
		    .find_next_button()
		    .expect("we could not find a valid button index!");
            }
	    
            // wait for next hand
	    // this is especially needed when there is only one player at the table
            let wait_duration = time::Duration::from_secs(1);
            thread::sleep(wait_duration);
	    
        }
        println!("about to send the gameover signal to the hub");
        // the game is ending, so tell that to the hub
        if let Some(hub_addr) = &self.hub_addr {
            // tell the hub that we left
            hub_addr.do_send(GameOver {
                table_name: self.name.clone(),
            });
        }
    }

    /// move the button to the next Player who is not sitting out
    /// if non can be found, then return false
    fn find_next_button(&mut self) -> Result<usize, &'static str> {
        for i in (self.button_idx + 1..9).chain(0..self.button_idx + 1) {
            //self.button_idx += 1;
            //self.button_idx %= 9; // loop back to 0 if we reach the end
            let button_spot = &mut self.players[i];
            if let Some(button_player) = button_spot {
                if button_player.is_sitting_out {
                    println!(
                        "Player at index {} is sitting out so cannot be the button",
                        i
                    );
                } else if button_player.money == 0 {
                    println!("Player at index {} has no money so cannot be the button", i);
                } else {
                    // We found a player who is not sitting out, so it is a valid
                    // button position
                    println!("found the button!");
                    return Ok(i);
                }
            }
        }
        Err("could not find a valid button")
    }

    fn handle_meta_actions(
	&mut self,
	incoming_meta_actions: &Arc<Mutex<VecDeque<MetaAction>>>,
	between_hands: bool,
	gamehand: Option<&GameHand>, // if we are between hands, then there won't be a gamehand
    ) {
        let mut meta_actions = incoming_meta_actions.lock().unwrap();	
        for _ in 0..meta_actions.len() {
            match meta_actions.pop_front().unwrap() {
                MetaAction::Chat(id, text) => {
                    // send the message to all players,
                    // appended by the player name
                    println!("chat message inside the game hand wow!");
		    if let Some(player_config) = self.player_ids_to_configs.get_mut(&id) {
			player_config.heart_beat = time::Instant::now(); // this counts as activity
			let message = object! {
			    msg_type: "chat".to_owned(),
			    player_name: player_config.name.clone(),
			    text: text,
                        };
			PlayerConfig::send_group_message(&message.dump(), &self.player_ids_to_configs);
		    }
                }		
                MetaAction::Join(player_config, password) => {
                    // add a new player to the table
                    let cloned_config = player_config.clone(); // clone in case we need to send back
                    println!(
                        "handling join meta action for {:?} inside table = {:?}",
                        cloned_config.id, &self.name
                    );
                    match self.add_human(player_config, password) {
                        Ok(index) => {
                            println!("Joining table at index: {}", index);
			    self.send_game_state(gamehand, false);
                        }
                        Err(err) => {
                            // we were unable to add the player
                            println!("unable to join table: {:?}", err);
                            if let Some(hub_addr) = &self.hub_addr {
                                // tell the hub that we left
                                hub_addr.do_send(Returned {
                                    config: cloned_config,
                                    reason: ReturnedReason::FailureToJoin(err),
                                });
                            }
                        }
                    }
                }
                MetaAction::Leave(id) => {
                    println!(
                        "handling leave meta action for {:?} inside table = {:?}. between hands = {}",
                        id, &self.name, between_hands
                    );
                    if let Some(config) = self.player_ids_to_configs.remove(&id) {
                        // note: we don't remove the player from self.players quite yet,
                        // we use the lack of the config to indicate to the table during a street
                        // that a player has left. If they were active at the time, this information
                        // needs to be taken into account
                        let message = object! {
                            msg_type: "player_left".to_owned(),
                            name: config.name.clone(),
                        };
                        PlayerConfig::send_specific_message(
                            &message.dump(),
                            id,
                            &self.player_ids_to_configs,
                        );

                        if let Some(hub_addr) = &self.hub_addr {
                            // tell the hub that we left
                            hub_addr.do_send(Returned {
                                config,
                                reason: ReturnedReason::Left,
                            });
                        }
                    } else {
                        // should not normally happen, but check for Some() to be safe
                        // Perhaps if the client sent many leave messages before them being responded to
                        println!("\n\nA leave message was received for a player that no longer has a config!")
                    }
                }
                MetaAction::SetPlayerName(id, new_name) => {
		    if let Some(player_config) = self.player_ids_to_configs.get_mut(&id) {
			player_config.name = Some(new_name.to_string());
			player_config.send_player_name();			
		    }
		    
                }
                MetaAction::SendPlayerName(id) => {
		    if let Some(player_config) = self.player_ids_to_configs.get(&id) {
			player_config.send_player_name();
		    }
                }
                MetaAction::UpdateAddress(id, new_addr) => {
                    PlayerConfig::set_player_address(id, new_addr, &mut self.player_ids_to_configs);
		    self.send_game_state(gamehand, false);		    
                }
                MetaAction::TableInfo(addr) => {
		    println!("about to send table info to {:?}", addr);
		    let message = object! {
			"msg_type": "table_info".to_owned(),
			"table_name": self.name.to_owned(),
			"small_blind": self.small_blind,
			"big_blind": self.big_blind,
			"buy_in": self.buy_in,
			"max_players": self.max_players,
			"num_humans": self.players.iter().flatten().filter(|p| p.human_controlled).count(),
			"num_bots": self.players.iter().flatten().filter(|p| !p.human_controlled).count(),
		    };
                    addr.do_send(WsMessage(message.dump()));		    
                }
                MetaAction::ImBack(id) => {
                    for player in self.players.iter_mut().flatten() {
                        if player.id == id {
                            println!("player {} being set to is_sitting_out = false", id);
                            player.is_sitting_out = false;
                        }
                    }
		    if let Some(player_config) = self.player_ids_to_configs.get_mut(&id) {
			player_config.heart_beat = time::Instant::now(); // this counts as activity
		    }
		    self.send_game_state(gamehand, false);		    		    
                }
                MetaAction::SitOut(id) => {
                    for player in self.players.iter_mut().flatten() {
                        if player.id == id {
                            println!("player {} being set to is_sitting_out = true", id);
                            player.is_sitting_out = true;
                        }
                    }
		    self.send_game_state(gamehand, false);		    		    
                }
		MetaAction::Admin(id, admin_command) => {
		    if !between_hands {
			// put it back on the meta actions queue to be handled only between hands
			println!("put the admin_command back on the queue to handle between hands");
			meta_actions.push_back(MetaAction::Admin(id, admin_command));
		    } else {
			self.handle_admin_command(id, admin_command);
		    }
		}
            }
        }
    }

    fn handle_admin_command(&mut self, id: Uuid, admin_command: AdminCommand) {
	println!("handling admin_command in table: {:?}", admin_command);
	if self.admin_id != id {
	    // the player who entered the admin command is not the table's admin!
	    let message = object! {
		msg_type: "error".to_owned(),
		error: "not_admin".to_owned(),
                reason: "You cannot update a table that you are not the admin for.".to_owned(),
	    };
	    PlayerConfig::send_specific_message(
		&message.dump(),
		id,
		&self.player_ids_to_configs,
	    );
	    return;
	}
	
	if self.password.is_none() {
	    // only private (i.e. password-protected) table can be updated
	    let message = object! {
		msg_type: "error".to_owned(),
		error: "not_private".to_owned(),
                reason: "You cannot update a table that is not private.".to_owned(),
	    };
	    PlayerConfig::send_specific_message(
		&message.dump(),
		id,
		&self.player_ids_to_configs,
	    );
	    return;
	}
	
	let message = match admin_command {
	    AdminCommand::SmallBlind(new) => {
		self.small_blind = new;
		object! {
		    msg_type: "admin_success".to_owned(),
		    updated: "small_blind".to_owned(),
                    text: format!("The small blind has been changed to {}", new),
		}
	    },
	    AdminCommand::BigBlind(new) => {
		self.big_blind = new;
		object! {
		    msg_type: "admin_success".to_owned(),
		    updated: "big_blind".to_owned(),
                    text: format!("The big blind has been changed to {}", new),
		}
	    }		
	    AdminCommand::BuyIn(new) => {
		self.buy_in = new;
		object! {
		    msg_type: "admin_success".to_owned(),
		    updated: "buy_in".to_owned(),
                    text: format!("The buy in has been changed to {}", new),
		}
	    }		
	    AdminCommand::SetPassword(new) => {
		self.password = Some(new.clone());
		object! {
		    msg_type: "admin_success".to_owned(),
		    updated: "password".to_owned(),
                    text: format!("The password has been changed to {}", new),
		}
	    }
	    AdminCommand::ShowPassword => {
		let pass_str = if let Some(password) = &self.password {
		    format!("The password is {:?}", password)
		} else {
		   "The table has no password".to_string()
		};
		object! {
		    msg_type: "admin_success".to_owned(),
                    text: pass_str,
		}
	    }	    
	    AdminCommand::AddBot => {
		match self.add_bot("Bot".to_string()) {
		    Ok(_) => {
			object! {
			    msg_type: "admin_success".to_owned(),
			    updated: "bot_added".to_owned(),
			    text: "A bot has been added.".to_owned(),
			}
		    }
		    Err(err) => {
			object! {
			    msg_type: "error".to_owned(),
			    error: "unable_to_add_bot".to_owned(),
			    reason: err.to_string(),
			}
		    }
		}
	    },	
	    AdminCommand::RemoveBot => {
		let mut found = false;
		for player_spot in self.players.iter_mut() {
		    if let Some(player) = player_spot {
			if !player.human_controlled {
			    println!("remove the bot!");
			    self
				.player_ids_to_configs
				.remove(&player.id)
				.expect("how was the bot a player but not a config");
			    *player_spot = None;
			    found = true;
			    break;
			}
		    }
		}
		if found {
		    object! {
			msg_type: "admin_success".to_owned(),
			updated: "bot_removed".to_owned(),
			text: "A bot has been removed.".to_owned(),		    
		    }
		} else {
		    object! {
			msg_type: "error".to_owned(),
			error: "unable_to_remove_bot".to_owned(),
			reason: "Unable to remove a bot from the table.".to_owned(),
		    }
		}
	    }
	    AdminCommand::Restart => {
		// set every player to have the buy_in amount of money
		println!("inside restart");
		for player_spot in self.players.iter_mut() {
		    if let Some(player) = player_spot {
			player.money = self.buy_in;
		    }
		}
		object! {
		    msg_type: "admin_success".to_owned(),
		    updated: "game_restarted".to_owned(),
		    text: "The game has been restarted to its original state.".to_owned(),
		}
	    }
	};
	PlayerConfig::send_specific_message(
            &message.dump(),
            id,
            &self.player_ids_to_configs,
	);
    }
	
    fn transition(&mut self, gamehand: &mut GameHand) {
	gamehand.current_bet = 0;
	gamehand.index_to_act = None;
        match gamehand.street {
            Street::Preflop => {
                gamehand.street = Street::Flop;
                self.deal_flop(gamehand);
                println!(
                    "\n===========================\nFlop = {:?}\n===========================",
                    gamehand.flop
                );
            }
            Street::Flop => {
                gamehand.street = Street::Turn;
                self.deal_turn(gamehand);
                println!(
                    "\n==========================\nTurn = {:?}\n==========================",
                    gamehand.turn
                );
            }
            Street::Turn => {
                gamehand.street = Street::River;
                self.deal_river(gamehand);
                println!(
                    "\n==========================\nRiver = {:?}\n==========================",
                    gamehand.river
                );
            }
            Street::River => {
                gamehand.street = Street::ShowDown;
                println!(
                    "\n==========================\nShowDown!\n================================"
                );
            }
            Street::ShowDown => (), // we are already in the end street (from players folding during the street)
        }
	self.send_game_state(Some(gamehand), false);	
    }

    fn deal_hands(&mut self) {
        for player in self.players.iter_mut().flatten() {
            if player.is_active {
                for _ in 0..2 {
                    if let Some(card) = self.deck.draw_card() {
                        player.hole_cards.push(card)
                    } else {
                        panic!("The deck is out of cards somehow?");
                    }
                }
            }
        }
    }

    fn deal_flop(&mut self, gamehand: &mut GameHand) {
        let mut flop = Vec::<Card>::with_capacity(3);
        for _ in 0..3 {
            if let Some(card) = self.deck.draw_card() {
                flop.push(card)
            } else {
                panic!("we exhausted the deck somehow");
            }
        }
        gamehand.flop = Some(flop);
    }

    fn deal_turn(&mut self, gamehand: &mut GameHand) {
        gamehand.turn = self.deck.draw_card();
    }

    fn deal_river(&mut self, gamehand: &mut GameHand) {
        gamehand.river = self.deck.draw_card();
    }

    fn finish_hand(&mut self, gamehand: &mut GameHand) {
        if self.player_ids_to_configs.is_empty() {
            // the game is currently empty, so there is nothing to finish
            return;
        }
        // Notify players the hand is over
        let mut finish_hand_message = object! {
            msg_type: "finish_hand".to_owned()
        };

	let starting_idx = self.get_starting_idx();
	let settlements = gamehand.divvy_pots(&mut self.players, &self.player_ids_to_configs, starting_idx);
	println!("blah settlements = {:?}", settlements);
        let wait_time = 3*settlements.len();
        finish_hand_message["settlements"] = settlements.into();	
        PlayerConfig::send_group_message(&finish_hand_message.dump(), &self.player_ids_to_configs);
        
        let pause_duration = time::Duration::from_secs(wait_time.try_into().unwrap());
        thread::sleep(pause_duration);	
        // take the players' cards
        for player in self.players.iter_mut().flatten() {
            player.hole_cards.drain(..);
        }
    }

    /// play a single hand of poker
    /// returns a bool indicating if the hand was "actually" played.
    /// because if there are < 2 active players, there is nothing to play
    fn play_one_hand(
        &mut self,
        incoming_actions: &Arc<Mutex<HashMap<Uuid, PlayerAction>>>,
        incoming_meta_actions: &Arc<Mutex<VecDeque<MetaAction>>>,
    ) -> bool {
        println!("inside of play(). button_idx = {:?}", self.button_idx);
        let mut gamehand = GameHand::default();
	let mut num_active = 0;
        for player in self.players.iter_mut().flatten() {
            if player.money == 0 {
                player.is_active = false;
            } else {
		// note: even sitting_out players start as active
		// since they might need to pay their blinds still
                player.is_active = true;
		num_active += 1;
            }
        }
        if self.player_ids_to_configs.len() < 1 || num_active < 2 {
	    // not enough players or active players to play a hand,
	    // send a game state indicating that the same is suspended,
	    // and return false to the main loop.
	    let game_suspended = true;
	    self.send_game_state(Some(&gamehand), game_suspended);		    
            return false;
        }

	let message = object! {
	    msg_type: "new_hand".to_owned(),
	    hand_num: self.hand_num,
	    button_index: self.button_idx,
        };
	PlayerConfig::send_group_message(&message.dump(), &self.player_ids_to_configs);
	
	// drain any lingering actions from a previous hand
        let mut actions = incoming_actions.lock().unwrap();
	actions.drain();
	std::mem::drop(actions); // give back the lock
	
	self.send_game_state(Some(&gamehand), false);	
        self.deck.shuffle();
        self.deal_hands();

        println!("players = {:?}", self.players);

        while gamehand.street != Street::ShowDown {
	    // before each street, set the player's last action to None
            for player in self.players.iter_mut().flatten() {
		player.last_action = None;
            }
	    
            let finished =
                self.play_street(incoming_actions, incoming_meta_actions, &mut gamehand);
            // pause for a second for dramatic effect heh
            let pause_duration = time::Duration::from_secs(2);
            thread::sleep(pause_duration);
	    
            if finished {
                // if the game is over from players folding
                println!("\nGame is ending before showdown!");
                break;
            } else {
                // otherwise we move to the next street
                self.transition(&mut gamehand)
            }
        }
        // now we finish up and pay the pot to the winner
        self.finish_hand(&mut gamehand);
	true // the hand was indeed played
    }

    fn get_starting_idx(&self) -> usize {
        // the starting index is either the person one more from the button on most streets,
        // or 3 down on the preflop (since the blinds already had to buy in)
        // TODO: this needs to be smarter in small games
        let mut starting_idx = self.button_idx + 1;
        if starting_idx >= self.players.len() {
            starting_idx = 0;
        }
        starting_idx
    }

    /// this method returns a bool indicating whether the hand is over or not
    fn play_street(
        &mut self,
        incoming_actions: &Arc<Mutex<HashMap<Uuid, PlayerAction>>>,
        incoming_meta_actions: &Arc<Mutex<VecDeque<MetaAction>>>,
        gamehand: &mut GameHand,
    ) -> bool {
        // if a player is still active but has no remaining money (i.e. is all-in),
        let mut num_all_in = self
            .players
            .iter()
            .flatten() // skip over None values
            .filter(|player| player.is_all_in())
            .count();

        let mut num_active = self
            .players
            .iter()
            .flatten() // skip over None values
            .filter(|player| player.is_active)
            .count();
        if num_active < 2 {
            println!(
                "num_active players = {}, so we cannot play a hand!",
                num_active
            );
            return true;
        }

        if num_all_in + 1 == num_active {
            println!("only one person is not all in, so don't bother with the street!");
            return false;
        }

        // once every player is either all-in or settled, then we move to the next street
        let mut num_settled = 0; // keep track of how many players have put in enough chips to move on
        println!("num active players = {}", num_active);

        if num_settled > 0 {
            println!("num settled (i.e. all in players) = {}", num_settled);
            PlayerConfig::send_group_message(
                &format!("num settled (i.e. all in players) = {}", num_settled),
                &self.player_ids_to_configs,
            );
        }
	
        let starting_idx = self.get_starting_idx(); // which player starts the betting

        gamehand.street_contributions.insert(gamehand.street, [0;9]);
	
	let between_hands = false;		
	
	let mut hand_over = false;	
        // iterate over the players from the starting index to the end of the vec,
        // and then from the beginning back to the starting index
        for i in (starting_idx..9).chain(0..starting_idx).cycle() {
	    // handle meta actions once right at the beginning to be responsive to sitout messages for example
            self.handle_meta_actions(&incoming_meta_actions, between_hands, Some(gamehand));
	    
            // double check if any players left as a meta-action during the previous
            // player's turn. They should no longer be considered as active or all_in
            for player_spot in self.players.iter_mut() {
		if let Some(player) = player_spot {
                    if !self.player_ids_to_configs.contains_key(&player.id) {
			println!("player is no longer in the config");
			if player.is_all_in() {
                        num_all_in -= 1;
			}
			if player.is_active {
                            player.deactivate(); // technically redundant I guess since setting to None later
                            num_active -= 1;
			}
			*player_spot = None;
                    }
		}
            }
	    
            if num_active == 1 {
                println!("Only one active player left so lets break the steet loop");
                // end the street and indicate to the caller that the hand is finished
		hand_over = true;
                break;
            }
            if num_settled + num_all_in == num_active {
                println!(
                    "everyone is ready to go to the next street! num_settled = {}",
                    num_settled
                );
                // end the street and indicate to the caller that the hand is going to the next street
                break;
            }

	    if let Some(player) = &self.players[i]  {
		println!("Player = {:?}, i = {}", player, i);		
		if !(player.is_active && player.money > 0) {
		    // if the player is not active with money, they can't do anything.
                    continue;
		}
	    } else {
                // no one sitting in this spot
                continue;
	    }
	    
	    gamehand.index_to_act = Some(i);
	    self.send_game_state(Some(&gamehand), false);
	    	    
            let action = self.get_and_validate_action(
                incoming_actions,
                incoming_meta_actions,
                gamehand,
		i
            );

	    println!("action = {:?}", action);
	    let player_cumulative = gamehand.street_contributions.get_mut(&gamehand.street).unwrap()[i];
            // now that we have gotten the current player's action and handled
            // any meta actions, we are free to respond and mutate the player
            // so we re-borrow it as mutable
            let player = self.players[i].as_mut().unwrap();
	    player.last_action = Some(action);
            match action {
                PlayerAction::PostSmallBlind(amount) => {	
                    player.money -= amount;		    	    
                    // regardless if the player couldn't afford it, the new street bet is the big blind
                    gamehand.current_bet = self.small_blind;
                    let all_in = if player.is_all_in() {
                        num_all_in += 1;
                        true
                    } else {
                        false
                    };
                    gamehand.contribute(i, player.id, amount, all_in);
                }
                PlayerAction::PostBigBlind(amount) => {
                    player.money -= amount;		    		    
                    // regardless if the player couldn't afford it, the new street bet is the big blind
                    gamehand.current_bet = self.big_blind;
                    let all_in = if player.is_all_in() {
                        num_all_in += 1;
                        true
                    } else {
                        false
                    };
                    gamehand.contribute(i, player.id, amount, all_in);
                    // note: we dont count the big blind as a "settled" player,
                    // since they still get a chance to act after the small blind
                }
                PlayerAction::Fold => {
                    player.deactivate();
                    num_active -= 1;
                }
                PlayerAction::SitOut => {
                    player.deactivate();
                    num_active -= 1;
                }
                PlayerAction::Check => {
                    num_settled += 1;
                }
                PlayerAction::Call => {
                    let difference = gamehand.current_bet - player_cumulative;
                    let (amount, all_in) = if difference >= player.money {
                        println!("you have to put in the rest of your chips");
                        num_all_in += 1;
			(player.money, true)
                    } else {
                        num_settled += 1;
			(difference, false)
                    };
                    player.money -= amount;		    
                    gamehand.contribute(i, player.id, amount, all_in);
		    
                }
                PlayerAction::Bet(new_bet) => {
                    let difference = new_bet - player_cumulative;
                    println!("difference = {}", difference);
                    gamehand.current_bet = new_bet;
                    player.money -= difference;		    		    
                    let all_in = if player.is_all_in() {
                        println!("Just bet the rest of our money!");
                        num_all_in += 1;
                        num_settled = 0;
                        true
                    } else {
                        num_settled = 1;
                        false
                    };
                    gamehand.contribute(i, player.id, difference, all_in);
                }
            }
        };
	self.send_game_state(Some(&gamehand), false);	
	hand_over
    }
    
    /// if the player is a human, then we look for their action in the incoming_actions hashmap
    /// this value is set by the table hub when handling a message from a player client
    fn get_action_from_player(
        &self,
        incoming_actions: &Arc<Mutex<HashMap<Uuid, PlayerAction>>>,
        player: &Player,
    ) -> Option<PlayerAction> {
        if player.human_controlled {
            let mut actions = incoming_actions.lock().unwrap();
            if let Some(action) = actions.get_mut(&player.id) {
                println!("Player: {:?} has action {:?}", player.id, action);
                let value = *action;
                actions.remove(&player.id); // wipe this action so we don't repeat it next time
                Some(value)
            } else {
                None
            }
        } else {
            let num = rand::thread_rng().gen_range(0..100);
            match num {
                0..=20 => Some(PlayerAction::Fold),
                21..=55 => Some(PlayerAction::Check),
                56..=70 => {
                    let amount: u32 = if player.money <= 100 {
                        // just go all in if we are at 10% starting
                        player.money
                    } else {
                        rand::thread_rng().gen_range(1..player.money / 2_u32)
                    };
                    Some(PlayerAction::Bet(amount))
                }
                _ => Some(PlayerAction::Call),
            }
        }
    }

    fn get_and_validate_action(
        &mut self,
        incoming_actions: &Arc<Mutex<HashMap<Uuid, PlayerAction>>>,
        incoming_meta_actions: &Arc<Mutex<VecDeque<MetaAction>>>,
        gamehand: &GameHand,
	index: usize
    ) -> PlayerAction {
        // if it isnt valid based on the current bet and the amount the player has already contributed,
        // then it loops
        // position is our spot in the order, with 0 == small blind, etc
	
        // we sleep a little bit each time so that the output doesnt flood the user at one moment
        let pause_duration = time::Duration::from_secs(1);
        thread::sleep(pause_duration);

	// note: several times in this method we access player within a scope, so that
	// we can call handle_meta_actions in between. Since that method wants to modify self.players,
	// we cannot have one borrowed at the same time.
	// I used to handle this via cloning the player, but that didn't seem satisfying, especially
	// since the player object contains a vec of Cards (this could be changed to an array of two opt<cards>
	// if that seemed better in the future to bring back the clone() in a lighter way)
	// I don't know if this is somewhat common, or if I have coded myself into a corner...
	let player_id = {
	    let player = self.players[index].as_ref().unwrap();	   	
            if gamehand.street == Street::Preflop && gamehand.current_bet == 0 {
		// collect small blind!
		return PlayerAction::PostSmallBlind(cmp::min(self.small_blind, player.money));
            } else if gamehand.street == Street::Preflop && gamehand.current_bet == self.small_blind {
		// collect big blind!
		return PlayerAction::PostBigBlind(cmp::min(self.big_blind, player.money));
            }

	    let player_cumulative = gamehand.street_contributions.get(&gamehand.street).unwrap()[index];	
	    let prompt = if gamehand.current_bet > player_cumulative {
		let diff = gamehand.current_bet - player_cumulative;
		format!("Enter action ({} to call): ", diff)
	    } else {
		format!("Enter action (current bet = {}): ", gamehand.current_bet)
	    };
	    let message = object! {
		msg_type: "prompt".to_owned(),
		prompt: prompt,
        current_bet: gamehand.current_bet
	    };
	    PlayerConfig::send_specific_message(
		&message.dump(),
		player.id,
		&self.player_ids_to_configs,
	    );
	    player.id
	};
        let mut action = None;
        let mut attempts = 0;
        let retry_duration = time::Duration::from_secs(1); // how long to wait between trying again
	let between_hands = false;		
        while attempts < 45 && action.is_none() {
            // the first thing we do on each loop is handle meta action
            // this lets us display messages in real-time without having to wait until after the
            // current player gives their action
            self.handle_meta_actions(&incoming_meta_actions, between_hands, Some(gamehand));
	    {
		let player = self.players[index].as_ref().unwrap();	   	
		let player_cumulative = gamehand.street_contributions.get(&gamehand.street).unwrap()[index];
		if player.human_controlled {
		    // we don't need to count the attempts at getting a response from a computer
		    // TODO: the computer can give a better than random guess at a move
		    // Currently it might try to check when it has to call for example,
		    attempts += 1;
		}
		if player.is_sitting_out {
		    println!("player is sitting out, so sitout/fold");
		    action = Some(PlayerAction::SitOut);
		    break;
		}
		if !self.player_ids_to_configs.contains_key(&player.id) {
		    // the config no longer exists for this player, so they must have left
		    println!("player config no longer exists, so the player must have left");
		    action = Some(PlayerAction::Fold);
		    break;
		}

		println!("Attempting to get player action on attempt {:?}", attempts);
		match self.get_action_from_player(incoming_actions, &player) {
		    None => {
			// we give the user a second to place their action
			thread::sleep(retry_duration);
		    }

		    Some(PlayerAction::Fold) => {
			if gamehand.current_bet <= player_cumulative {
			    // if the player has put in enough then no sense folding
			    if player.human_controlled {
				println!("you said fold but we will let you check!");
				let message = json::object! {
				msg_type: "error".to_owned(),
				error: "invalid_action".to_owned(),
				reason: "You said fold but we will let you check!".to_owned(),
				};
				PlayerConfig::send_specific_message(
				    &message.dump(),
				    player.id,
				    &self.player_ids_to_configs,
				);
			    }
			    action = Some(PlayerAction::Check);
			} else {
			    action = Some(PlayerAction::Fold);
			}
		    }
		    Some(PlayerAction::Check) => {
			//println!("Player checks!");
			if gamehand.current_bet > player_cumulative {
			    // if the current bet is higher than this player's bet
			    if player.human_controlled {
				let message = json::object! {
				msg_type: "error".to_owned(),
				error: "invalid_action".to_owned(),
				reason: "You can't check since there is a bet!!".to_owned(),
				};
				PlayerConfig::send_specific_message(
				    &message.dump(),
				    player.id,
				    &self.player_ids_to_configs,
				);
			    }
			    continue;
			}
			action = Some(PlayerAction::Check);
		    }
		    Some(PlayerAction::Call) => {
			if gamehand.current_bet <= player_cumulative {
			    if gamehand.current_bet != 0 {
				// if the street bet isn't 0 then this makes no sense
				println!("should we even be here???!");
			    }
			    let message = json::object! {
				msg_type: "error".to_owned(),
				error: "invalid_action".to_owned(),
				reason: "There is nothing for you to call!".to_owned()
			    };
			    PlayerConfig::send_specific_message(
				&message.dump(),
				player.id,
				&self.player_ids_to_configs,
			    );
			    // we COULD let them check, but better to wait for a better action
			    continue;
			}
			action = Some(PlayerAction::Call);
		    }
		    Some(PlayerAction::Bet(new_bet)) => {
			if gamehand.current_bet < player_cumulative {
			    // will this case happen?
			    println!("this should not happen!");
			    continue;
			}
			if new_bet > player.money + player_cumulative {
			    println!("cant bet more than you have");
			    let message = json::object! {
				msg_type: "error".to_owned(),
				error: "invalid_action".to_owned(),
				reason:"You can't bet more than you have!!".to_owned(),
			    };
			    PlayerConfig::send_specific_message(
				&message.dump(),
				player.id,
				&self.player_ids_to_configs,
			    );
			    continue;
			}
			if new_bet <= gamehand.current_bet {
			    println!("new bet must be larger than current");
			    let message = json::object! {
				msg_type: "error".to_owned(),
				error: "invalid_action".to_owned(),
				reason: "the new bet must be larger than the current bet!".to_owned(),
			    };
			    PlayerConfig::send_specific_message(
				&message.dump(),
				player.id,
				&self.player_ids_to_configs,
			    );
			    continue;
			}
			action = Some(PlayerAction::Bet(new_bet));
		    }
		    other => {
			action = other;
		    }
		}
	    }
        }
        // if we got a valid action, then we can return it,
        // otherwise, we timed out, so sit out
        if let Some(action) = action {
	    if let Some(player_config) = self.player_ids_to_configs.get_mut(&player_id) {
		// the fact that we received an action tells us to update the active heartbeat		
		player_config.heart_beat = time::Instant::now();
	    }
	    action
        } else {
	    // send a meta action (to ourself) that this player should be sitting out
            incoming_meta_actions
                .lock()
                .unwrap()
                .push_back(MetaAction::SitOut(player_id));
            PlayerAction::SitOut
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logic::card::{Rank, Suit};
    use crate::logic::deck::RiggedDeck;    
    use std::collections::HashMap;

    #[test]
    fn add_bot() {
        let mut table = Table::default();
        let name = "Mr Bot".to_string();
        let index = table.add_bot(name);
        assert_eq!(index.unwrap(), 0); // the first position to be added to is index 0
        assert_eq!(table.players.len(), 9);
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 1);
        assert!(!table.players[0].as_ref().unwrap().human_controlled);
    }

    #[test]
    fn add_human() {
        let mut table = Table::default();
        let id = uuid::Uuid::new_v4();
        let name = "Human".to_string();
        let settings = PlayerConfig::new(id, Some(name), None);
        table.add_human(settings, None).expect("could not add user");
        assert_eq!(table.players.len(), 9);
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 1);
        assert!(table.players[0].as_ref().unwrap().human_controlled);
    }

    /// test that a player can join with the correct password
    #[test]
    fn add_human_password_success() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        let password = "123".to_string();
        table.password = Some(password.clone());

        let id = uuid::Uuid::new_v4();
        let name = "Human".to_string();
        let settings = PlayerConfig::new(id, Some(name), None);

        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Join(settings, Some(password)));

        table.handle_meta_actions(&cloned_meta_actions, true, None);
        assert_eq!(table.players.len(), 9);
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 1);
        assert!(table.players[0].as_ref().unwrap().human_controlled);
    }

    /// test that a player can NOT join with the incorrect password
    #[test]
    fn add_human_password_fail() {
        let mut table = Table::default();
        //let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));

        table.password = Some("123".to_string());

        let id = uuid::Uuid::new_v4();
        let name = "Human".to_string();
        let settings = PlayerConfig::new(id, Some(name), None);

        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Join(settings, Some("345".to_string())));

        table.handle_meta_actions(&incoming_meta_actions, true, None);
	
        assert_eq!(table.players.len(), 9);
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 0); // did not make it in
    }

    /// test that a player can NOT join without providing a password
    #[test]
    fn add_human_password_missing() {
        let mut table = Table::default();
        //let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));

        table.password = Some("123".to_string());

        let id = uuid::Uuid::new_v4();
        let name = "Human".to_string();
        let settings = PlayerConfig::new(id, Some(name), None);

        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Join(settings, None)); // no password passed in

        table.handle_meta_actions(&incoming_meta_actions, true, None);	

        assert_eq!(table.players.len(), 9);
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 0); // did not make it in
    }

    /// if we set max_players, then trying to add anyone past that point will
    /// not work
    #[test]
    fn max_players_in_game() {
        let mut table = Table::default();
        let max_players = 3;
        table.max_players = max_players;

        // we TRY to add 5 bots
        for i in 0..5 {
            let name = format!("Bot {}", i);
            let index = table.add_bot(name);
            if i < max_players {
                assert_eq!(index.unwrap() as u8, i);
            } else {
                // above max_players, the returned index should be None
                // i.e. the player was not added to the game
                assert!(index.is_err());
            }
        }
        assert_eq!(table.players.len(), 9); // len of players always simply 9

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        // but only max_players players are in the game at the end
        assert_eq!(some_players as u8, max_players);
    }

    /// the small blind folds, so the big blind should win and get paid
    #[test]
    fn instant_fold() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

        // set the action that player2 folds
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Fold);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 1004);
        assert_eq!(table.players[1].as_ref().unwrap().money, 996);
    }

    /// the small blind calls, the big blind checks to the flop
    /// the small blind bets on the flop, and the big blind folds
    #[test]
    fn call_check_bet_fold() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

        // set the action that player2 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Call);
        // player1 checks
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Check);

        // wait for the flop
        let wait_duration = time::Duration::from_secs(7);
        thread::sleep(wait_duration);

        // player2 bets on the flop
        println!("now sending the flop actions");
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(10));
        // player1 folds
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 992);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1008);
    }

    /// the small blind bets, the big blind folds
    #[test]
    fn pre_flop_bet_fold() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // set the action that player2 bets
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(22));
        // player1 folds
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 992);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1008);
    }

    /// if the big blind player doesn't have enough to post the big blind amount,
    /// the current bet still goes up to the big blind
    #[test]
    fn big_blind_not_enough_money() {
        let mut deck = RiggedDeck::new();

        // we want the button/big blind to win
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Heart,
        });
        // now the small blind's hole cards
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Club,
        });
        // now the full run out
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::Queen,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);

        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button/big blind
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();
        table.players[0].as_mut().unwrap().money = 3; // set the player to have less than the norm 8 BB

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.5)); 
	
        // set the action that player (small blind) bets,
        // even though player1 is already all-in, so the BB can only 3 win bucks
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(22));

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 6);
        assert_eq!(table.players[1].as_ref().unwrap().money, 997);
    }

    /// the small blind bets, the big blind calls
    /// the small blind bets on the flop, and the big blind folds
    #[test]
    fn bet_call_bet_fold() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });
	
	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
		      
        // set the action that player2 bets
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(22));
        // player1 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Call);

        // wait for the flop
        let wait_duration = time::Duration::from_secs(7);
        thread::sleep(wait_duration);

        // player2 bets on the flop
        println!("now sending the flop actions");
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(10));
        // player1 folds
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 978);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1022);
    }

    /// the small blind goes all in and the big blind calls
    #[test]
    fn all_in_call() {
        let mut deck = RiggedDeck::new();

        // we want the button/big blind to lose for testing purposes
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Club,
        });

        // now the small blind's hole cards
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Heart,
        });

        // now the full run out
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::Queen,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // set the action that player2 bets
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(1000));
        // player1 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // the small blind won
        assert_eq!(table.players[0].as_ref().unwrap().money, 0);
        assert_eq!(table.players[1].as_ref().unwrap().money, 2000);
    }

    /// the small blind bets and the big blind calls
    /// this call makes the big blind go all-in
    #[test]
    fn call_all_in() {
        let mut deck = RiggedDeck::new();

        // we want the button/big blind to lose for testing purposes
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Club,
        });

        // now the small blind's hole cards
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Heart,
        });

        // now the full run out
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::Queen,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        table.players[0].as_mut().unwrap().money = 500; // set the player to have less money

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // set the action that player2 bets
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(500));
        // player1 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // the small blind won
        assert_eq!(table.players[0].as_ref().unwrap().money, 0);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1500);
    }

    /// the small blind bets and the big blind calls
    /// this call makes the big blind go all-in
    /// In this test, the original bet is more than the big blind even has,
    /// and the big blind wins only the amount it puts in (500)
    #[test]
    fn small_stack_call_all_in() {
        let mut deck = RiggedDeck::new();

        // we want the button/big blind to win for testing purposes
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Heart,
        });

        // now the small blind's losing hole cards
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Club,
        });

        // now the full run out
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::Queen,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button/big
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Big".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        table.players[0].as_mut().unwrap().money = 500; // set the player to have less money

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Small".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // set the action that player2 bets a bunch
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(1000));
        // player1 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // the big blind caller won, but only doubles its money
        assert_eq!(table.players[0].as_ref().unwrap().money, 1000);

        // the small blind only loses half
        assert_eq!(table.players[1].as_ref().unwrap().money, 500);
    }

    /// if a player goes all-in, then can only win as much as is called up to that amount,
    /// even if other players keep playing and betting during this hand
    /// In this test, the side pot is won by the short stack, then the remaining is won
    /// by another player
    #[test]
    fn outright_side_pot() {
        let mut deck = RiggedDeck::new();

        // we want the button to win his side pot
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Diamond,
        });

        // the small blind will win the main pot against the big blind
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ten,
            suit: Suit::Heart,
        });

        // the big blind loses
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Four,
            suit: Suit::Heart,
        });

        // now the full run out
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Eight,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::Nine,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Button".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();
        // set the button to have less money so there is a side pot
        table.players[0].as_mut().unwrap().money = 500;

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Small".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();

        // player3 will start as the big blind
        let id3 = uuid::Uuid::new_v4();
        let name3 = "Big".to_string();
        let settings3 = PlayerConfig::new(id3, Some(name3), None);
        table.add_human(settings3, None).unwrap();

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 3);
        assert!(table.players[0].as_ref().unwrap().human_controlled);
        assert!(table.players[1].as_ref().unwrap().human_controlled);
        assert!(table.players[2].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // the button goes all in with the short stack
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Bet(500));
        // the small blind goes all in with a full stack
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(1000));
        // the big blind calls the full all-in
        incoming_actions
            .lock()
            .unwrap()
            .insert(id3, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // the button won the side pot
        assert_eq!(table.players[0].as_ref().unwrap().money, 1500);

        // the small blind won the remainder
        assert_eq!(table.players[1].as_ref().unwrap().money, 1000);

        // the big blind lost everything
        assert_eq!(table.players[2].as_ref().unwrap().money, 0);
    }

    /// if a player goes all-in, then can only win as much as is called up to that amount,
    /// even if other players keep playing and betting during this hand
    /// In this test, the small stack ties with one of the other players, so the main spot should be split
    /// This other player beats the third player in the side pot
    #[test]
    fn tie_side_pot() {
        let mut deck = RiggedDeck::new();

        // we want the button to win the main pot
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Diamond,
        });

        // the small blind will tie the main and win the side pot against the big blind
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Heart,
        });

        // the big blind loses
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Four,
            suit: Suit::Heart,
        });

        // now the full run out
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Eight,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::Nine,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Button".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();
        // set the button to have less money so there is a side pot
        table.players[0].as_mut().unwrap().money = 500;

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Small".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();

        // player3 will start as the big blind
        let id3 = uuid::Uuid::new_v4();
        let name3 = "Big".to_string();
        let settings3 = PlayerConfig::new(id3, Some(name3), None);
        table.add_human(settings3, None).unwrap();

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 3);
        assert!(table.players[0].as_ref().unwrap().human_controlled);
        assert!(table.players[1].as_ref().unwrap().human_controlled);
        assert!(table.players[2].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // the button goes all in with the short stack
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Bet(500));
        // the small blind goes all in with a full stack
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(1000));
        // the big blind calls the full all-in
        incoming_actions
            .lock()
            .unwrap()
            .insert(id3, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // the button won the side pot
        assert_eq!(table.players[0].as_ref().unwrap().money, 750);

        // the small blind won the remainder
        assert_eq!(table.players[1].as_ref().unwrap().money, 1750);

        // the big blind lost everything
        assert_eq!(table.players[2].as_ref().unwrap().money, 0);
    }

    /// if a player goes all-in, then can only win as much as is called up to that amount,
    /// even if other players keep playing and betting during this hand
    /// In this test, the main pot is won by the small stack, then medium stack wins a separate
    /// side pot, and finally, the rest of the chips are won by a third player
    #[test]
    fn multiple_side_pots() {
        let mut deck = RiggedDeck::new();

        // we want the button to win the main pot
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Diamond,
        });

        // the small blind will win the remaining
        deck.push(Card {
            rank: Rank::Six,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Six,
            suit: Suit::Heart,
        });

        // the big blind loses
        deck.push(Card {
            rank: Rank::Two,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Four,
            suit: Suit::Heart,
        });

        // UTG wins the second side pot
        deck.push(Card {
            rank: Rank::Queen,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Queen,
            suit: Suit::Heart,
        });

        // now the full run out
        deck.push(Card {
            rank: Rank::Three,
            suit: Suit::Diamond,
        });
        deck.push(Card {
            rank: Rank::Eight,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::Nine,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Club,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Button".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();
        // set the button to have less money so there is a side pot
        table.players[0].as_mut().unwrap().money = 500;

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Small".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();

        // player3 will start as the big blind
        let id3 = uuid::Uuid::new_v4();
        let name3 = "Big".to_string();
        let settings3 = PlayerConfig::new(id3, Some(name3), None);
        table.add_human(settings3, None).unwrap();

        // player4 will start as UTG
        let id4 = uuid::Uuid::new_v4();
        let name4 = "UTG".to_string();
        let settings4 = PlayerConfig::new(id4, Some(name4), None);
        table.add_human(settings4, None).unwrap();
        // set UTG to have medium money so there is a second side pot
        table.players[3].as_mut().unwrap().money = 750;

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 4);
        assert!(table.players[0].as_ref().unwrap().human_controlled);
        assert!(table.players[1].as_ref().unwrap().human_controlled);
        assert!(table.players[2].as_ref().unwrap().human_controlled);
        assert!(table.players[3].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // UTG goes all in with the medium stack
        incoming_actions
            .lock()
            .unwrap()
            .insert(id4, PlayerAction::Bet(750));
        // the button calls (and thus goes all in with the short stack)
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Call);
        // the small blind goes all in with a full stack
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(1000));
        // the big blind calls the full all-in
        incoming_actions
            .lock()
            .unwrap()
            .insert(id3, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // the button won the side pot
        assert_eq!(table.players[0].as_ref().unwrap().money, 2000);

        // the small blind won the remainder
        assert_eq!(table.players[1].as_ref().unwrap().money, 500);

        // the big blind lost everything
        assert_eq!(table.players[2].as_ref().unwrap().money, 0);

        // UTG won the second side pot
        assert_eq!(table.players[3].as_ref().unwrap().money, 750);
    }

    /// can we pass a hand limit of 2 and the game comes to an end
    #[test]
    fn hand_limit() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human1".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play(&cloned_actions, &cloned_meta_actions, Some(2));
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.5)); 
	
        // set the action that player2 folds
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Fold);

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(10.5)); 
	println!("ADDING THE FOLD OUTSIDE GAME\n\n");	
        // then player1 folds next hand
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the money balances out
        assert_eq!(table.players[0].as_ref().unwrap().money, 1000);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1000);
    }
    
    /// the game should end after N hands if there are no human players in the game
    /// even if there is no hand limit or a high hand limit
    /// Note: in this test there are no players period, but the game will still count each check
    /// as a hand "played", so we can check that the game ends with the proper count
    #[test]
    fn end_early() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        for i in 0..2 {
            let name = format!("Bot {}", i);
            let index = table.add_bot(name);
	    assert!(index.is_ok());
        }
	
        let handler = std::thread::spawn(move || {
            // we start the game with None hand limit!
            table.play(&cloned_actions, &cloned_meta_actions, None);
            table // return the table back
        });

        // get the game back from the thread
        let table = handler.join().unwrap();

        // check that the game ended with 1 more than the limit turns "played"
        assert_eq!(table.hand_num, NON_HUMAN_HANDS_LIMIT + 1);
    }

    /// check that the button moves around properly
    /// we play 4 hands with 3 players with everyone folding whenever it gets to them,
    /// Note: we sleep several seconds in the test to let the game finish its hand in its thread,
    /// so the test is brittle to changes in wait durations within the table.
    /// If this test starts failing in the future, it is likely just a matter of tweaking the sleep
    /// durations
    #[test]
    fn button_movement() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human2".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();

        let id3 = uuid::Uuid::new_v4();
        let name3 = "Human3".to_string();
        let settings3 = PlayerConfig::new(id3, Some(name3), None);
        table.add_human(settings3, None).unwrap();

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 3);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let num_hands = 4;
        let handler = std::thread::spawn(move || {
            table.play(&cloned_actions, &cloned_meta_actions, Some(num_hands));
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.5)); 
	
        // id3 should not have to act as the big blind
        println!("\n\nsetting 1!");
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Fold);

        // wait for next hand
        let wait_duration = time::Duration::from_secs(9);
        thread::sleep(wait_duration);

        println!("\n\nsetting 2!");
        // id1 should not have to act as the big blind
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Fold);
        incoming_actions
            .lock()
            .unwrap()
            .insert(id3, PlayerAction::Fold);

        // wait for next hand
        thread::sleep(wait_duration);

        println!("\n\nsetting 3!");
        // id2 should not have to act as the big blind
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);
        incoming_actions
            .lock()
            .unwrap()
            .insert(id3, PlayerAction::Fold);

        // wait for next hand
        thread::sleep(wait_duration);

        // We should be back to the beginning with the button,
        // so id1 should be the button, and id3 should be the big blind
        // id3 should not have to act as the big blind
        println!("\n\nsetting 4!");
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Fold);
        //incoming_actions.lock().unwrap().insert(id4, PlayerAction::Fold);

        let table = handler.join().unwrap();

        // Everyone lost their small blind and won someone else's small blind
        // then in the last hand, id3 won the small blind from id2
        assert_eq!(table.players[0].as_ref().unwrap().money, 1000);
        assert_eq!(table.players[1].as_ref().unwrap().money, 996);
        assert_eq!(table.players[2].as_ref().unwrap().money, 1004);
    }

    /// the small blind calls, the big blind checks to the flop
    /// the small blind bets on the flop, and the big blind folds
    /// a player joins during the hand, and it works fine
    #[test]
    fn join_mid_hand() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human2".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // set the action that player2 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Call);
        // player1 checks
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Check);

        // a new player joins the game
        let id3 = uuid::Uuid::new_v4();
        let name3 = "Human3".to_string();
        let settings3 = PlayerConfig::new(id3, Some(name3), None);

        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Join(settings3, None)); // no password needed

        // wait for the flop
        let wait_duration = time::Duration::from_secs(8);
        thread::sleep(wait_duration);

        // player2 bets on the flop
        println!("now sending the flop actions");
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(10));
        // player1 folds
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Fold);

        // get the game back from the thread
        let table = handler.join().unwrap();

        // there is another player now
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 3);

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 992);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1008);
        assert_eq!(table.players[2].as_ref().unwrap().money, 1000);
        assert!(!table.players[2].as_ref().unwrap().is_active);
    }

    /// player1 has the best hand, but chooses to sit out mid hand,
    /// This leads to a fold and player2 winning the pot
    /// It doesn't actually matter what the hands are, since it doesn't go to showdown
    #[test]
    fn sit_out() {
        let mut deck = RiggedDeck::new();

        // we want the button to have the best hand
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Diamond,
        });

        // the small blind player2 wins regardless
        deck.push(Card {
            rank: Rank::Six,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Five,
            suit: Suit::Heart,
        });

        // the flop
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human2".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        // both players not sitting out to start
        let not_sitting_out = table
            .players
            .iter()
            .flatten()
            .filter(|x| !x.is_sitting_out)
            .count();
        assert_eq!(not_sitting_out, 2);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.2)); 
	
        // set the action that player2 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Call);
        // player1 checks
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Check);

        // wait for the flop
        let wait_duration = time::Duration::from_secs(8);
        thread::sleep(wait_duration);

        // player2 bets on the flop
        println!("now sending the flop actions");
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Bet(10));

        // player1 sit out META action, which folds and ends the hand
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::SitOut(id1));
	
        // get the game back from the thread
        let table = handler.join().unwrap();

        // one player sitting out
        let not_sitting_out = table
            .players
            .iter()
            .flatten()
            .filter(|x| !x.is_sitting_out)
            .count();
        assert_eq!(not_sitting_out, 1);

        // check that the money changed hands
        assert_eq!(table.players[0].as_ref().unwrap().money, 992);
        assert_eq!(table.players[1].as_ref().unwrap().money, 1008);
        assert!(!table.players[0].as_ref().unwrap().is_active);
    }
    /// player1 has the best hand, but chooses to leave out mid hand,
    /// This leads to a fold and player2 winning the pot
    /// It doesn't actually matter what the hands are, since it doesn't go to showdown
    #[test]
    fn leave() {
        let mut deck = RiggedDeck::new();

        // we want the button to have the best hand
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Diamond,
        });

        // the small blind player2 wins regardless
        deck.push(Card {
            rank: Rank::Six,
            suit: Suit::Club,
        });
        deck.push(Card {
            rank: Rank::Five,
            suit: Suit::Heart,
        });

        // the flop
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Heart,
        });
        deck.push(Card {
            rank: Rank::Ace,
            suit: Suit::Spade,
        });
        deck.push(Card {
            rank: Rank::King,
            suit: Suit::Heart,
        });

        let mut table = Table::default();
        table.deck = Box::new(deck);
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human2".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();

        // flatten to get all the Some() players
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 2);
        assert!(table.players[0].as_ref().unwrap().human_controlled);

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we wait before adding the leave meta action
        thread::sleep(time::Duration::from_secs_f32(1.2)); 
	
        // set the action that player2 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id2, PlayerAction::Call);

        // player1 leave, which folds and ends the hand
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Leave(id1));

        // get the game back from the thread
        let table = handler.join().unwrap();

        // flatten to get all the Some() players
        // now there are only one
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 1);
        assert_eq!(table.player_ids_to_configs.len(), 1);

        // check that the money changed hands
        assert!(table.players[0].is_none()); // the spot is empty now
        assert_eq!(table.players[1].as_ref().unwrap().money, 1008);
    }

    /// if someone who is not the admin attempts an admin command, it does not work
    #[test]
    fn not_admin() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_blind = table.small_blind + 1;
	assert_eq!(table.small_blind, new_blind - 1); // duh
	
        // need the id for the admin command
	// but we do not set the game's admin
        let id = uuid::Uuid::new_v4();
	
	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::SmallBlind(new_blind)));

	
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	assert_eq!(table.small_blind, new_blind - 1); // nothing changed	
    }
    
    /// test that the admin can change the small blind with a meta action
    #[test]
    fn admin_small_blind() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_blind = table.small_blind + 1;
	assert_eq!(table.small_blind, new_blind - 1); // duh
	
        // need the id for the admin command
        let id = uuid::Uuid::new_v4();
	table.admin_id = id; // set the game's admin

	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::SmallBlind(new_blind)));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	assert_eq!(table.small_blind, new_blind);	       
    }

    /// test that admin commands do not work for a game that is private (i.e. has a password)
    #[test]
    fn admin_no_password() {
        let mut table = Table::default();
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_blind = table.small_blind + 1;
	assert_eq!(table.small_blind, new_blind - 1); // duh
	
        // need the id for the admin command
        let id = uuid::Uuid::new_v4();
	table.admin_id = id; // set the game's admin

	assert!(table.password.is_none()); // make sure no password is set
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::SmallBlind(new_blind)));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	assert_eq!(table.small_blind, new_blind - 1); // still
    }
    
    /// test that the admin can change the big blind with a meta action
    #[test]
    fn admin_big_blind() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_blind = table.big_blind + 1;
	assert_eq!(table.big_blind, new_blind - 1);	       
	
        // need the id for the admin command
        let id = uuid::Uuid::new_v4();
	table.admin_id = id; // set the game's admin

	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::BigBlind(new_blind)));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	assert_eq!(table.big_blind, new_blind);	       
    }

    /// test that the admin can change the buy in with a meta action
    #[test]
    fn admin_buy_in() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_buy_in = table.buy_in + 1;
	assert_eq!(table.buy_in, new_buy_in - 1);	       
	
        // need the id for the admin command
        let id = uuid::Uuid::new_v4();
	table.admin_id = id; // set the game's admin

	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());

        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::BuyIn(new_buy_in)));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	assert_eq!(table.buy_in, new_buy_in);	       
    }

    /// test that the admin can change the password in with a meta action
    #[test]
    fn admin_password() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_password = "new_password".to_string();
	assert_ne!(table.password, Some(new_password.clone()));
	
        // need the id for the admin command
        let id = uuid::Uuid::new_v4();
	table.admin_id = id; // set the game's admin

	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::SetPassword(new_password.clone())));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	assert_eq!(table.password, Some(new_password));	
    }

    /// test that the admin can add and remove bots with a meta action
    /// in this test, we add three bots, then remove one.
    /// the empty seat is at index 0
    #[test]
    fn admin_bots() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();
	let new_buy_in = table.buy_in + 1;
	assert_eq!(table.buy_in, new_buy_in - 1);	       

        assert_eq!(table.player_ids_to_configs.len(), 0); // no player configs
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 0); // no players

        // need the id for the admin command
        let id = uuid::Uuid::new_v4();
	table.admin_id = id; // set the game's admin

	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::AddBot));
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::AddBot));	    
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::AddBot));	    
        table.handle_meta_actions(&cloned_meta_actions, true, None);
        assert_eq!(table.player_ids_to_configs.len(), 3); // 3 player configs
        let some_players = table.players.iter().flatten().count();
        assert_eq!(some_players, 3); // 3 players
	
	for i in 0..3 {
            assert!(!table.players[i].as_ref().unwrap().human_controlled); // a bot
	}

	// now remove a bot
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id, AdminCommand::RemoveBot));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
        assert_eq!(table.player_ids_to_configs.len(), 2); // 2 player configs
	// the player_ids_to_configs mapping no longer contains the id for the bot at index 0
	assert!(table.players[0].as_ref().is_none());
    }

    /// test that the admin can restart the table. this brings all players to the buy_in amount of money
    #[test]
    fn admin_restart() {
        let mut table = Table::default();
        //let _incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        //let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        let id1 = uuid::Uuid::new_v4();
        let name1 = "1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();
        table.players[0].as_mut().unwrap().money = 500;

        let id2 = uuid::Uuid::new_v4();
        let name2 = "2".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
	
        // need the id for the admin command
	table.admin_id = id1; // set the game's admin

	// only game's with a password (private) can be updated
	table.password = Some("arbitrary".to_string());
	
	let new_buy_in = 4321; // arbitrary
	table.buy_in = new_buy_in;
	
        incoming_meta_actions
            .lock()
            .unwrap()
            .push_back(MetaAction::Admin(id1, AdminCommand::Restart));
        table.handle_meta_actions(&cloned_meta_actions, true, None);
	// check that the players have the new_buy_in amount of money
	assert_eq!(table.players[0].as_mut().unwrap().money, new_buy_in);
	assert_eq!(table.players[1].as_mut().unwrap().money, new_buy_in);	
    }

    /// even if a player is_sitting_out, they still are obliged to pay the blinds as
    /// they come around.
    #[test]
    fn sitting_out_pay_blinds() {
        let mut table = Table::default();
        let incoming_actions = Arc::new(Mutex::new(HashMap::<Uuid, PlayerAction>::new()));
        let incoming_meta_actions = Arc::new(Mutex::new(VecDeque::<MetaAction>::new()));
        let cloned_actions = incoming_actions.clone();
        let cloned_meta_actions = incoming_meta_actions.clone();

        // player1 will start as the button
        let id1 = uuid::Uuid::new_v4();
        let name1 = "Human1".to_string();
        let settings1 = PlayerConfig::new(id1, Some(name1), None);
        table.add_human(settings1, None).unwrap();

        // player2 will start as the small blind
        let id2 = uuid::Uuid::new_v4();
        let name2 = "Human2".to_string();
        let settings2 = PlayerConfig::new(id2, Some(name2), None);
        table.add_human(settings2, None).unwrap();
	
        // player3 will start as the big blind
        let id3 = uuid::Uuid::new_v4();
        let name3 = "Human3".to_string();
        let settings2 = PlayerConfig::new(id3, Some(name3), None);
        table.add_human(settings2, None).unwrap();

	// player2 is_sitting_out
        table.players[1].as_mut().unwrap().is_sitting_out = true;
	// player3 is_sitting_out
        table.players[2].as_mut().unwrap().is_sitting_out = true;
	
	// confirm we have two sitting out players
        let num_sitting_out = table.players.iter().flatten().filter(|p| p.is_sitting_out).count();
        assert_eq!(num_sitting_out, 2);	

        let handler = std::thread::spawn(move || {
            table.play_one_hand(&cloned_actions, &cloned_meta_actions);
            table // return the table back
        });

	// sleep so we dont drain the actions accidentally right at the beginning of play_one_hand
        thread::sleep(time::Duration::from_secs_f32(0.5)); 
	
        // set the action that player1 calls
        incoming_actions
            .lock()
            .unwrap()
            .insert(id1, PlayerAction::Call);

        // get the game back from the thread
        let table = handler.join().unwrap();

	// each sitting out player should pay their blinds and then fold,
	// and player1 will win the blinds
        assert_eq!(table.players[0].as_ref().unwrap().money, 1012);
        assert_eq!(table.players[1].as_ref().unwrap().money, 996);
        assert_eq!(table.players[1].as_ref().unwrap().money, 996);	
    }
    
}
