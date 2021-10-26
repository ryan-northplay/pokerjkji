use std::iter;
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use rand::Rng;
use rand::seq::SliceRandom;

#[derive(Debug, Copy, Clone, EnumIter)]
enum Rank {
    ACE = 1,
    TWO = 2,
    THREE = 3,
    FOUR = 4,
    FIVE = 5,
    SIX = 6,
    SEVEN = 7,
    EIGHT = 8,
    NINE = 9,
    TEN = 10,
    JACK = 11,
    QUEEN = 12,
    KING = 13,

}

#[derive(Debug, Copy, Clone, EnumIter)]
enum Suit {
    CLUB,
    DIAMOND,
    HEART,
    SPADE
}

#[derive(Debug, Copy, Clone)]
struct Card {
    rank: Rank,
    suit: Suit,
}

struct Deck {
    cards: Vec<Card>,
    top: usize, // index that we deal the next card from
}

impl Deck {
    fn new () -> Self {
	// returns a new unshuffled deck of 52 cards 
	let mut cards = Vec::<Card>::with_capacity(52);
	for rank in Rank::iter() {
	    for suit in Suit::iter() {
		cards.push(Card{rank, suit});
	    }
	}
	Deck {cards, top: 0}
    }

    fn shuffle(&mut self) {
	// shuffle the deck of cards
	self.cards.shuffle(&mut rand::thread_rng());
	self.top = 0;
    }

    fn draw_card(&mut self) -> Option<Card> {
	// take the top card from the deck and move the index of the top of the deck
	if self.top == self.cards.len() {
	    // the deck is exhausted, no card to give
	    None
	}
	else {
	    let card = self.cards[self.top];	    
	    self.top += 1;
	    Some(card)
	}
    }
}



enum PlayerAction {
    Fold,
    Check,
    Bet(f64),
    Call,
    //Raise(u32), // i guess a raise is just a bet really?
}

#[derive(Debug)]
struct Player {
    name: String,
    hand: Vec<Card>,    
    is_active: bool,
    money: f64,
}

impl Player {
    fn new(name: String) -> Self {
	Player {
	    name: name,
	    hand: Vec::<Card>::with_capacity(2),
	    is_active: true,
	    money: 1000.0, // let them start with 1000 for now
	}
    }
    
    fn pay(&mut self, payment: f64) {
	println!("getting paid inside {:?}", self);
	self.money += payment
    }

    fn deactivate(&mut self) {
	self.is_active = false;
    }
}

#[derive(Debug, PartialEq)]
enum Street {
    Preflop,
    Flop,
    Turn,
    River,
    End
}

struct GameHand<'a> {
    deck: &'a mut Deck,
    players: &'a mut Vec<Player>,
    num_active: usize,
    button_idx: usize, // the button index dictates where the action starts
    street: Street,
    pot: f64, // current size of the pot
    flop: Option<Vec<Card>>,
    turn: Option<Card>,
    river: Option<Card>,
}

impl <'a> GameHand<'a> {
    fn new (deck: &'a mut Deck, players: &'a mut Vec<Player>, button_idx: usize) -> Self {
	let num_active = players.iter().filter(|player| player.is_active).count(); // active to start the hand	    	
	GameHand {
	    deck: deck,
	    players: players,
	    num_active: num_active,	    
	    button_idx: button_idx,
	    street: Street::Preflop,
	    pot: 0.0,
	    flop: None,
	    turn: None,
	    river: None
	}

    }
    
    fn transition(&mut self) {
	match self.street {
	    Street::Preflop => {
	    	self.street = Street::Flop;
		self.deal_flop();		
	    },
	    Street::Flop => {
	    	self.street = Street::Turn;
		self.deal_turn();		
	    }
	    Street::Turn => {
	    	self.street = Street::River;
		self.deal_river();		
	    }
	    Street::River => {
	    	self.street = Street::End;
	    }
	    Street::End => () // we are already in the end street (from players folding during the street)
	}
    }

    
    fn deal_hands(&mut self) {
	for player in self.players.iter_mut() {
	    if player.is_active {
		for _ in 0..2 {
		    if let Some(card) = self.deck.draw_card() {
			player.hand.push(card)
		    } else {
			panic!();
		    }
		}
	    }
	}
    }
    
    fn deal_flop(&mut self) {
	let mut flop = Vec::<Card>::with_capacity(3);
	for _ in 0..3{
	    if let Some(card) = self.deck.draw_card() {
		flop.push(card)
	    } else {
		panic!();
	    }
	}
	self.flop = Some(flop);
    }
    
    fn deal_turn(&mut self) {
	self.turn = self.deck.draw_card();	    
    }
    
    fn deal_river(&mut self) {
	self.river = self.deck.draw_card();	    	    
    }

    fn finish(&mut self) {
	let mut remaining = Vec::<&mut Player>::new();
	for player in self.players.iter_mut() {
	    if player.is_active {
		println!("found an active player remaining");
		remaining.push(player);
	    }
	}
	// winners is a vec of bools the same length of the remaining where we keep track of
	// which ones are a winner and entitled to part of the pot
	//let mut winner_flags = [false; remaining.len()];
	let mut winner_flags: Vec<bool> = iter::repeat(false).take(remaining.len()).collect();
	match remaining.len() {
	    1 => {
		winner_flags[0] = true;
	    }
	    _ => {
		// TODO: how do we pick a winner from multiple ppl at showdown
		winner_flags[0] = true;		
	    }
	}
	// divy the pot to all the winners	
	let num_winners = winner_flags.iter().filter(|flag| **flag).count();
	let payout = self.pot as f64 / num_winners as f64;
	
	for (player, is_winner) in remaining.iter_mut().zip(winner_flags.iter()) {
	    if *is_winner {
		player.pay(payout);
	    }
	}

	// take the players' cards
	for player in self.players.iter_mut() {
	    // todo: is there any issue with calling drain if they dont have any cards?
	    player.hand.drain(..);		
	}
    }

    fn play(&mut self) {
	println!("inside of play()");
	self.deck.shuffle();
	for card in self.deck.cards.iter() {
	    println!("{:?}", card);
	}
	self.deal_hands();
	
	println!("self.players = {:?}", self.players);	
	while self.street != Street::End {
	    println!("\nStreet is {:?}", self.street);
	    self.play_street();
	    if self.num_active == 1 {
		// if the game is over from players folding
		break;
	    } else {
		// otherwise we move to the next street
		self.transition();
	    }
 	}
	// now we finish up and pay the pot to the winner
	self.finish();	
    }

    fn get_starting_idx(&self) -> usize {
	// the starting index is either the person one more from the button on most streets,
	// or 3 down on the preflop (since the blinds already had to buy in)
	// TODO: this needs to be smarter in small games
	let mut starting_idx = self.button_idx + 1;
	if starting_idx as usize >= self.players.len() {
	    starting_idx += 1;
	}
	starting_idx
    }
    
    fn play_street(&mut self) {
	let mut street_bet: f64 = 0.0;
	let mut cumulative_bets = vec![0.0; self.players.len()]; // each index keeps track of that players' contribution this street

	// TODO: if preflop then collect blinds
	if self.street is Preflop
	    let (left, right) = self.players.split_at_mut(starting_idx);
	    for (i, mut player) in right.iter_mut().chain(left.iter_mut()).enumerate() {
	
	
	let starting_idx = self.get_starting_idx(); // which player starts the betting
	let mut num_settled = 0; // keeps track of how many players have either checked through or called the last bet (or made the last bet)
	// if num_settled == self.active, then we are good to go to the next street 
	
	let mut loop_count = 0;
	'street: loop {
	    /*
	    if loop_count > 2 {
		break;
	    }
	     */
	    loop_count += 1;
	    println!("loop count = {}", loop_count);
	    
	    // iterate over the players from the starting index to the end of the vec, and then from the beginning back to the starting index
	    let (left, right) = self.players.split_at_mut(starting_idx);
	    for (i, mut player) in right.iter_mut().chain(left.iter_mut()).enumerate() {
		let player_cumulative = cumulative_bets[i];
		println!("Player = {:?}, i = {}", player, i);
		println!("Current pot = {:?}, Current size of the bet = {:?}, and this player has put in {:?} so far",
			 self.pot,
			 street_bet,
			 player_cumulative);
		if player.is_active {
		    println!("Player is active");		    
		    // this loop can keep going while it waits for a proper action
		    // get an validate an action from the player
		    match GameHand::get_and_validate_action(&player, street_bet, player_cumulative) {
			PlayerAction::Fold => {
			    println!("Player folds!");			    
			    player.deactivate();	    
			    self.num_active -= 1;
			}
			PlayerAction::Check => {
			    println!("Player checks!");			    			    
			    num_settled += 1;
			},
			PlayerAction::Call => {
			    println!("Player calls!");			    			    			    
			    let difference = street_bet - player_cumulative;
			    if difference  > player.money {
				println!("you have to put in the rest of your chips");
				self.pot += player.money;
				cumulative_bets[i] += player.money;
				player.money = 0.0;				
				
			    } else {				
				self.pot += difference;
				cumulative_bets[i] += difference;				
				player.money -= difference;
			    }
			    num_settled += 1;
			},
			PlayerAction::Bet(new_bet) => {
			    println!("Player bets {}!", new_bet);			    			    			    			    
			    let difference = new_bet - player_cumulative;			    
			    self.pot += difference;
			    player.money -= difference;
			    street_bet = new_bet;
			    cumulative_bets[i] = new_bet;
			    num_settled = 1; // since we just bet more, we are the only settled player
			}
		    }
		}
		println!("num_active = {}, num_settled = {}", self.num_active, num_settled);
		if self.num_active == 1 {
		    println!("Only one active player left so lets break the steet loop");
		    break 'street;
		}
		if num_settled == self.num_active {
		    // every active player is ready to move onto the next street
		    println!("everyone is ready to go to the next street! num_settled = {}", num_settled);
		    break 'street;
		}
		
	    }
	}
    }
    

    fn get_action_from_user(player: &Player) -> PlayerAction {
	// will need UI here
	// for now do a random action
	
	let num = rand::thread_rng().gen_range(0..100);
	match num {
	    0..=15 => PlayerAction::Fold,
	    15..=39 => PlayerAction::Check,
	    40..=70 => PlayerAction::Bet(player.money), // bet it all baby!
	    _ => PlayerAction::Call,
	}
    }

    fn get_and_validate_action(player: &Player, street_bet: f64, player_cumulative: f64 ) -> PlayerAction {
	// if it isnt valid based on the current bet and the amount the player has already contributed, then it loops
	let mut action;
	'valid_check: loop {
	    action = GameHand::get_action_from_user(player);
	    match action {
		PlayerAction::Fold => {		
		    //println!("Player folds!");
		    if street_bet <= player_cumulative {
			// if the player has put in enough then no sense folding
			//println!("you said fold but we will let you check!");
			action = PlayerAction::Check;
		    }
		    break 'valid_check;						    
		}
		PlayerAction::Check => {
		    //println!("Player checks!");				
		    if street_bet > player_cumulative {
			// if the current bet is higher than this players bet
			//println!("invalid action!");
			continue;
		    }
		    break 'valid_check;				
		},
		PlayerAction::Call => {
		    //println!("Player calls!");				
		    if street_bet <= player_cumulative {
			//println!("invalid action!");
			continue;
		    }
		    break 'valid_check;				
		},
		PlayerAction::Bet(new_bet) => {
		    //println!("Player bets {}!", new_bet);								
		    if street_bet < player_cumulative {
			// will this case happen?
			println!("this should not happen!");
			continue;
		    }
		    if new_bet - player_cumulative  > player.money {
			//println!("you cannot bet more than you have!");
			continue;
		    }
		    if new_bet <= street_bet {
			//println!("the new bet has to be larger than the current bet!");
			continue;
		    }
		    break 'valid_check;				
		}
	    }
	}
	action
    }
}

struct Game {
    deck: Deck,
    players: Vec<Player>,
    button_idx: usize, // index of the player with the button
    small_blind: u32,
    big_blind: u32,
}


impl Game {
    fn new() -> Self {	
	Game {
	    deck: Deck::new(),
	    players: Vec::<Player>::with_capacity(9),
	    small_blind: 4,
	    big_blind: 8,
	    button_idx: 0
	}
    }

    fn add_player(&mut self, player: Player) {
	self.players.push(player)
    }

    fn play_one_hand(&mut self) {
	let mut game_hand = GameHand::new(&mut self.deck, &mut self.players, self.button_idx
	);
	game_hand.play();	    
    }

    fn play(&mut self) {
	loop {
	    self.play_one_hand();
	    // TODO: do we need to add or remove any players?
	    
	    // TODO: what happens with the button_idx  if a player leaves
	    self.button_idx += 1; // and modulo length
	    if self.button_idx as usize >= self.players.len() {
		self.button_idx = 0;
	    }
	    
	    break; // TODO: when should the game actually end
	}
    }
}

fn main() {
    println!("Hello, world!");    
    let mut game = Game::new();
    let num_players = 2;
    for i in 0..num_players {
	let name = format!("Mr {}", i);
	game.add_player(Player::new(name));
    }
    game.play();
}