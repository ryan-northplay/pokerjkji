import React, { useRef } from "react";
import { drawTable, drawBackground } from "./drawTable";
import { Player } from "./player";
import { PlayerCard } from "./playerCard";
import { drawFrontCard } from "./drawCard";

const TableCanvas = props => {
    const canvasRef = useRef(null);

    function draw() {
        const canvas = canvasRef.current;
        if (canvas === null) return;

        const context = canvas.getContext('2d');
        const canvasW = canvas.parentNode.getBoundingClientRect().width;
        const canvasH = canvas.parentNode.getBoundingClientRect().height;

        canvas.width = canvasW;
        canvas.height = canvasH;

        // Draw Backgroud
        drawBackground(context, canvasW, canvasH);

        // Draw Table
        drawTable(context, canvasW, canvasH);

        if (props.gameState) {
            // Draw players
            for (let i = 0; i < props.gameState.max_players; i++) {
                let playerState = props.gameState.players[i];

                if (playerState === null) {
                    continue;
                }

                // the mapped index is always relative to the main player being at index 0
                let mapped_index = (playerState.index - props.gameState.your_index + 9) % 9;

                // Draw player
                let is_players_turn_to_act = props.gameState.index_to_act === playerState.index;

                let street_contributions = 0;

                if (props.gameState.street === "preflop") {
                    street_contributions = playerState.preflop_cont;
                } else if (props.gameState.street === "flop") {
                    street_contributions = playerState.flop_cont;
                } else if (props.gameState.street === "turn") {
                    street_contributions = playerState.turn_cont;
                } else if (props.gameState.street === "river") {
                    street_contributions = playerState.river_cont;
                }

                let player = new Player({
                    index: mapped_index,
                    name: playerState.player_name,
                    money: playerState.money,
                    action: playerState.last_action,
                    is_players_turn_to_act: is_players_turn_to_act,
                    street_contributions: street_contributions,
                    is_active: playerState.is_active
                });

                // If this is the player then show the player their cards
                if ("hole_cards" in props.gameState &&
                    props.gameState.hole_cards !== null &&
                    playerState.index === props.gameState.your_index) {
                    let holeCards = props.gameState.hole_cards;

                    var chars = holeCards.split("");

                    player.giveCards(
                        new PlayerCard(true, chars[0], chars[1]),
                        new PlayerCard(true, chars[2], chars[3])
                    );
                } else if (playerState.is_active && playerState.last_action !== "fold") {
                    player.giveCards(
                        new PlayerCard(false),
                        new PlayerCard(false)
                    );
                }

                player.draw(context, canvasW, canvasH);
                player.drawChips(context, canvasW, canvasH);

                if (props.gameState.button_idx === playerState.index) {
                    // Draw Button
                    player.drawButton(context, canvasW, canvasH);
                }
            }

            var size = Math.min(canvasW, canvasH);
            let card_size = 0.075 * size;
            let card_margin = card_size / 4;
            let card_start = canvasW / 2 - (6 * card_margin + 5 * card_size) / 2;

            // table cards
            if ("flop" in props.gameState) {
                const chars = props.gameState.flop.split("");
                drawFrontCard(
                    context,
                    card_start + card_margin,
                    canvasH / 2 - card_size,
                    chars[0],
                    chars[1],
                    card_size
                );
                drawFrontCard(
                    context,
                    card_start + 2 * card_margin + card_size,
                    canvasH / 2 - card_size,
                    chars[2],
                    chars[3],
                    card_size
                );
                drawFrontCard(
                    context,
                    card_start + 3 * card_margin + 2 * card_size,
                    canvasH / 2 - card_size,
                    chars[4],
                    chars[5],
                    card_size
                );
            }
            if ("turn" in props.gameState) {
                const chars = props.gameState.turn.split("");
                drawFrontCard(
                    context,
                    card_start + 4 * card_margin + 3 * card_size,
                    canvasH / 2 - card_size,
                    chars[0],
                    chars[1],
                    card_size
                );
            }
            if ("river" in props.gameState) {
                const chars = props.gameState.river.split("");
                drawFrontCard(
                    context,
                    card_start + 5 * card_margin + 4 * card_size,
                    canvasH / 2 - card_size,
                    chars[0],
                    chars[1],
                    card_size
                );
            }

            // Draw pots
            if ("pots" in props.gameState) {
                context.font = "bold 18px arial";
                context.textAlign = "center";
                context.fillStyle = "white";
                context.fillText("Pot(s): " + props.gameState.pots, canvasW / 2, canvasH / 2 + 80);
            }

            if ("current_bet" in props.gameState) {
                context.font = "bold 18px arial";
                context.textAlign = "center";
                context.fillStyle = "white";
                context.fillText(
                    "Current bet: " + props.gameState.current_bet,
                    canvasW / 2,
                    canvasH / 2 + 110
                );
            }
        }
    }

    // Add event listener
    window.addEventListener("resize", draw);

    // Call handler right away so state gets updated with initial window size
    draw();

    return <canvas ref={canvasRef} className={props.className} />
};

export default TableCanvas;
