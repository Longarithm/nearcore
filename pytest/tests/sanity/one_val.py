#!/usr/bin/env python3
# Creates a genesis config with two block producers, and kills one right away after
# launch. Makes sure that the other block producer can produce blocks with chunks and
# process transactions. Makes large-ish number of block producers per shard to minimize
# the chance of the second block producer occupying all the seats in one of the shards

import sys, time, base58, random
import pathlib

sys.path.append(str(pathlib.Path(__file__).resolve().parents[2] / 'lib'))

from cluster import start_cluster
from configured_logger import logger
import utils
from transaction import sign_payment_tx

EPOCH_LENGTH = 10
TIMEOUT = 180

genesis_change = [
    ["minimum_validators_per_shard", 1], ["min_gas_price", 0],
    ["max_inflation_rate", [0, 1]], ["epoch_length", EPOCH_LENGTH],
    ["block_producer_kickout_threshold", 60],
    ["chunk_producer_kickout_threshold", 60],
    ["validators", 0, "amount", "110000000000000000000000000000000"],
    [
        "records", 0, "Account", "account", "locked",
        "110000000000000000000000000000000"
    ], ["total_supply", "4060000000000000000000000000000000"]
]

# give more stake to the boot node so that it can produce the blocks alone
nodes = start_cluster(
    2, 1, 8, None, genesis_change, {
        0: {
            "tracked_shards_config": "AllShards"
        },
        1: {
            "tracked_shards_config": "AllShards"
        }
    })
utils.wait_for_blocks(nodes[0], target=3)
nodes[1].kill()

act_to_val = [0, 0, 0]
ctx = utils.TxContext(act_to_val, nodes)

last_balances = [x for x in ctx.expected_balances]

sent_height = -1
caught_up_times = 0

# Wait for the killed validator be kicked out, otherwise we will keep missing
# chunks due to being not validated by that validator.
utils.wait_for_blocks(nodes[0], target=(EPOCH_LENGTH * 4) + 3)

for height, hash_ in utils.poll_blocks(nodes[0],
                                       timeout=TIMEOUT,
                                       poll_interval=0.1):
    logger.info(f'Got to height {height}')

    if ctx.get_balances() == ctx.expected_balances:
        logger.info('Balances caught up, took %s blocks, moving on',
                    height - sent_height)
        ctx.send_moar_txs(hash_, 10, use_routing=True)
        sent_height = height
        caught_up_times += 1
    else:
        assert height <= sent_height + 30, ('Balances before: {before}\n'
                                            'Expected balances: {expected}\n'
                                            'Current balances: {current}\n'
                                            'Sent at height: {height}').format(
                                                before=last_balances,
                                                expected=ctx.expected_balances,
                                                current=ctx.get_balances(),
                                                height=sent_height)

    if caught_up_times == 3:
        break
