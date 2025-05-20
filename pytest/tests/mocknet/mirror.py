#!/usr/bin/env python3
"""
Cli tool for managing the mocknet instances.
"""
from argparse import ArgumentParser, Action
import pathlib
from rc import pmap
import re
import sys
import time

from mirror_commands import CommandContext, clear_scheduled_cmds, hard_reset_cmd, \
    init_cmd, list_scheduled_cmds, new_test_cmd, reset_cmd, run_env_cmd, run_remote_cmd, \
    run_remote_upload_file, start_nodes_cmd, stop_nodes_cmd, update_binaries_cmd, update_config_cmd

sys.path.append(str(pathlib.Path(__file__).resolve().parents[2] / 'lib'))

from configured_logger import logger


def restart_cmd(ctx: CommandContext):
    targeted = ctx.get_targeted()
    pmap(lambda node: node.stop_neard_runner(), targeted)
    if ctx.args.upload_program:
        pmap(lambda node: node.upload_neard_runner(), targeted)
    pmap(lambda node: node.start_neard_runner(), targeted)


def stop_runner_cmd(ctx: CommandContext):
    targeted = ctx.get_targeted()
    pmap(lambda node: node.stop_neard_runner(), targeted)


def make_backup_cmd(ctx: CommandContext):
    args = ctx.args
    if not args.yes:
        print(
            'this will stop all nodes and create a new backup of their home dirs. continue? [yes/no]'
        )
        if sys.stdin.readline().strip() != 'yes':
            sys.exit()

    if args.backup_id is None:
        print('please enter a backup ID:')
        args.backup_id = sys.stdin.readline().strip()
        if re.match(r'^[0-9a-zA-Z.][0-9a-zA-Z_\-.]+$', args.backup_id) is None:
            sys.exit('invalid backup ID')
        if args.description is None:
            print('please enter a description (enter nothing to skip):')
            description = sys.stdin.readline().strip()
            if len(description) > 0:
                args.description = description

    pmap(
        lambda node: node.neard_runner_make_backup(
            backup_id=args.backup_id, description=args.description),
        ctx.get_targeted_with_schedule_ctx())


def stop_traffic_cmd(ctx: CommandContext):
    ctx.traffic_generator.with_schedule_ctx(
        ctx.schedule_ctx).neard_runner_stop()


def start_traffic_cmd(ctx: CommandContext):
    nodes = ctx.nodes
    traffic_generator = ctx.traffic_generator

    if traffic_generator is None:
        logger.warning('No traffic node selected. Change filters.')
        return
    if not all(pmap(lambda node: node.neard_runner_ready(),
                    ctx.get_targeted())):
        logger.warning(
            'not all nodes are ready to start yet. Run the `status` command to check their statuses'
        )
        return
    pmap(
        lambda node: node.with_schedule_ctx(ctx.schedule_ctx).
        neard_runner_start(), nodes)
    # Wait for the nodes to be up if not scheduling
    if not ctx.is_scheduled():
        logger.info("waiting for validators to be up")
        pmap(lambda node: node.wait_node_up(), nodes)
        logger.info(
            "waiting a bit after validators started before starting traffic")
        time.sleep(10)
    # TODO: maybe add 20 seconds delay to the schedule command to allow the other nodes to start
    traffic_generator.with_schedule_ctx(ctx.schedule_ctx).neard_runner_start(
        batch_interval_millis=ctx.args.batch_interval_millis)
    if not ctx.is_scheduled():
        logger.info(
            f'test running. to check the traffic sent, try running "curl --silent http://{traffic_generator.ip_addr()}:{traffic_generator.neard_port()}/metrics | grep near_mirror"'
        )


def amend_binaries_cmd(ctx: CommandContext):
    args = ctx.args
    pmap(
        lambda node: node.neard_runner_update_binaries(
            args.neard_binary_url, args.epoch_height, args.binary_idx),
        ctx.get_targeted_with_schedule_ctx())


class ParseFraction(Action):

    def __call__(self, parser, namespace, values, option_string=None):
        pattern = r"(\d+)/(\d+)"
        match = re.match(pattern, values)
        if not match:
            parser.error(f"Invalid input '{values}'. Expected format 'i/n'.")
        numerator = int(match.group(1))
        denominator = int(match.group(2))
        setattr(namespace, self.dest, (numerator, denominator))


def build_parser():
    parser = ArgumentParser(description='Control a mocknet instance')
    parser.add_argument('--mocknet-id',
                        type=str,
                        help='''
                        Identifier of the mocknet instance to use. Can be used instead of specifying
                        `chain-id`, `start-height` and `unique-id`.
                        ''')
    parser.add_argument('--chain-id', type=str)
    parser.add_argument('--start-height', type=int)
    parser.add_argument('--unique-id', type=str)
    parser.add_argument('--local-test', action='store_true')
    parser.add_argument('--host-type',
                        type=str,
                        choices=['all', 'nodes', 'traffic'],
                        default='all',
                        help='Type of hosts to select')
    parser.add_argument('--host-filter',
                        type=str,
                        help='Filter through the selected nodes using regex.')
    parser.add_argument('--select-partition',
                        action=ParseFraction,
                        type=str,
                        help='''
                        Input should be in the form of "i/n" where 0 < i <= n.
                        Select a group of hosts based on the division provided.
                        For i/n, it will split the selected hosts into n groups and select the i-th group.
                        Use this if you want to target just a partition of the hosts.'''
                       )

    subparsers = parser.add_subparsers(title='subcommands',
                                       description='valid subcommands',
                                       help='additional help')

    register_schedule_subcommands(subparsers)
    # register subcommands to root parser
    register_base_commands(subparsers)
    register_subcommands(subparsers)
    return parser


def register_schedule_subcommands(subparsers):
    schedule_parser = subparsers.add_parser('schedule',
                                            help='Manage scheduled commands.')
    subparsers = schedule_parser.add_subparsers(
        title='subcommands',
        description='Manage scheduled commands.',
        help='additional help')
    cmd_subparsers = subparsers.add_parser(
        'cmd', help='Schedule commands to run in the future.')
    cmd_subparsers.add_argument(
        '--schedule-in',
        required=True,
        type=str,
        help=
        'Schedule the command to run after the specified time. Can be in the format of "10s", "10m", "10h", "10d"'
    )
    cmd_subparsers.add_argument(
        '--schedule-id',
        type=str,
        help=
        'The id of the scheduled command. If not provided, random string will be generated.'
    )
    # Add all existing commands under 'cmd'
    cmd_subcommands = cmd_subparsers.add_subparsers(
        title='subcommands',
        description='valid subcommands',
        help='additional help',
        required=True)
    # register subcommands to schedule_subparsers
    register_subcommands(cmd_subcommands)

    list_subparsers = subparsers.add_parser('list',
                                            help='List all scheduled commands.')
    list_subparsers.add_argument(
        '--full',
        action='store_true',
        help='Show more details about the scheduled commands.')
    list_subparsers.set_defaults(func=list_scheduled_cmds)

    clear_subparsers = subparsers.add_parser(
        'clear', help='Clear all scheduled commands.')
    clear_subparsers.add_argument(
        '--filter',
        type=str,
        default='*',
        help='''Clear scheduled commands matching the regex.
    Use the ids provided by the list command including the ".timer" suffix.
    If not already, all values will be prefixed with 'mocknet-' to match the scheduled command name.
    If not provided, all scheduled commands will be cleared.''')
    clear_subparsers.set_defaults(func=clear_scheduled_cmds)


def register_base_commands(subparsers):
    init_parser = subparsers.add_parser('init-neard-runner',
                                        help='''
    Sets up the helper servers on each of the nodes. Doesn't start initializing the test
    state, which is done with the `new-test` command.
    ''')
    init_parser.add_argument('--neard-binary-url', type=str)
    init_parser.add_argument('--neard-upgrade-binary-url', type=str)
    init_parser.set_defaults(func=init_cmd)

    restart_parser = subparsers.add_parser(
        'restart-neard-runner',
        help='''Restarts the neard runner on all nodes.''')
    restart_parser.add_argument('--upload-program', action='store_true')
    restart_parser.set_defaults(func=restart_cmd, upload_program=False)

    stop_runner_parser = subparsers.add_parser(
        'stop-neard-runner', help='''Stops the neard runner on all nodes.''')
    stop_runner_parser.set_defaults(func=stop_runner_cmd)

    hard_reset_parser = subparsers.add_parser(
        'hard-reset',
        help='''Stops neard and clears all test state on all nodes.''')
    hard_reset_parser.add_argument('--neard-binary-url', type=str)
    hard_reset_parser.add_argument('--neard-upgrade-binary-url', type=str)
    hard_reset_parser.set_defaults(func=hard_reset_cmd)

    new_test_parser = subparsers.add_parser('new-test',
                                            help='''
    Sets up new state from the prepared records and genesis files with the number
    of validators specified. This calls neard amend-genesis to create the new genesis
    and records files, and then starts the neard nodes and waits for them to be online
    after computing the genesis state roots. This step takes a long time (a few hours).
    ''')
    new_test_parser.add_argument('--state-source', type=str, default='dump')
    new_test_parser.add_argument('--patches-path', type=str)
    new_test_parser.add_argument('--epoch-length', type=int)
    new_test_parser.add_argument('--num-validators', type=int)
    new_test_parser.add_argument('--num-seats', type=int)
    new_test_parser.add_argument('--new-chain-id', type=str)
    new_test_parser.add_argument('--genesis-protocol-version', type=int)
    new_test_parser.add_argument('--stateless-setup', action='store_true')
    new_test_parser.add_argument(
        '--gcs-state-sync',
        action='store_true',
        help=
        """Enable state dumper nodes to sync state to GCS. On localnet, it will dump locally."""
    )
    new_test_parser.add_argument('--yes', action='store_true')
    new_test_parser.set_defaults(func=new_test_cmd)

    status_parser = subparsers.add_parser(
        'status',
        help='''Checks the status of test initialization on each node''')
    status_parser.set_defaults(func=status_cmd)

    upload_file_parser = subparsers.add_parser('upload-file',
                                               help='''
        Upload a file or a directory on the hosts.
        Existing files are replaced.
        ''')
    upload_file_parser.add_argument('--src', type=str)
    upload_file_parser.add_argument('--dst', type=str)
    upload_file_parser.set_defaults(func=run_remote_upload_file)


def register_subcommands(subparsers):
    """
    This function registers the commands that can also be scheduled.
    Before adding a new command, make sure that is makes sense for it to be scheduled.
    Otherwise, just add it to the base commands.
    """
    update_config_parser = subparsers.add_parser(
        'update-config',
        help='''Update config.json with given flags for all nodes.''')
    update_config_parser.add_argument(
        '--set',
        help='''
        A key value pair to set in the config. The key will be interpreted as a
        json path to the config to be updated. The value will be parsed as json.   
        e.g.
        --set 'aaa.bbb.ccc=5'
        --set 'aaa.bbb.ccc="5"'
        --set 'aaa.bbb.ddd={"eee":6,"fff":"7"}' # no spaces!
        ''',
    )
    update_config_parser.set_defaults(func=update_config_cmd)

    start_traffic_parser = subparsers.add_parser(
        'start-traffic',
        help=
        'Starts all nodes and starts neard mirror run on the traffic generator.'
    )
    start_traffic_parser.add_argument(
        '--batch-interval-millis',
        type=int,
        help=
        '''Interval in millis between sending each mainnet block\'s worth of transactions.
        Without this flag, the traffic generator will try to match the per-block load on mainnet.
        So, transactions from consecutive mainnet blocks will be sent with delays
        between them such that they will probably appear in consecutive mocknet blocks.
        ''')
    start_traffic_parser.set_defaults(func=start_traffic_cmd)

    start_nodes_parser = subparsers.add_parser(
        'start-nodes',
        help='Starts all nodes, but does not start the traffic generator.')
    start_nodes_parser.set_defaults(func=start_nodes_cmd)

    stop_parser = subparsers.add_parser('stop-nodes',
                                        help='kill all neard processes')
    stop_parser.set_defaults(func=stop_nodes_cmd)

    stop_parser = subparsers.add_parser(
        'stop-traffic',
        help='stop the traffic generator, but leave the other nodes running')
    stop_parser.set_defaults(func=stop_traffic_cmd)

    backup_parser = subparsers.add_parser('make-backup',
                                          help='''
    Stops all nodes and haves them make a backup of the data dir that can later be restored to with the reset command
    ''')
    backup_parser.add_argument('--yes', action='store_true')
    backup_parser.add_argument('--backup-id', type=str)
    backup_parser.add_argument('--description', type=str)
    backup_parser.set_defaults(func=make_backup_cmd)

    reset_parser = subparsers.add_parser('reset',
                                         help='''
    The new_test command saves the data directory after the genesis state roots are computed so that
    the test can be reset from the start without having to do that again. This command resets all nodes'
    data dirs to what was saved then, so that start-traffic will start the test all over again.
    ''')
    reset_parser.add_argument('--yes', action='store_true')
    reset_parser.add_argument('--backup-id', type=str)
    reset_parser.set_defaults(func=reset_cmd)

    # It re-uses the same binary urls because it's quite easy to do it with the
    # nearcore-release buildkite and urls in the following format without commit
    # but only with the branch name:
    # https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/Linux/<branch-name>/neard"
    update_binaries_parser = subparsers.add_parser('update-binaries',
                                                   help='''
        Update the neard binaries by re-downloading them. The same urls are used.
        If you plan to restart the network multiple times, it is recommended to use
        URLs that only depend on the branch name. This way, every time you build,
        you will not need to amend the URL but just run update-binaries.''')
    update_binaries_parser.set_defaults(func=update_binaries_cmd)

    amend_binaries_parsers = subparsers.add_parser('amend-binaries',
                                                   help='''
        Add or override the neard URLs by specifying the epoch height or index if you have multiple binaries.

        If the network was started with 2 binaries, the epoch height for the second binary can be randomly assigned
        on each host. Use caution when updating --epoch-height so that it will not add a binary in between the upgrade
        window for another binary.''')

    amend_binaries_parsers.add_argument('--neard-binary-url',
                                        type=str,
                                        required=True,
                                        help='URL to the neard binary.')
    group = amend_binaries_parsers.add_mutually_exclusive_group(required=True)
    group.add_argument('--epoch-height',
                       type=int,
                       help='''
        The epoch height where this binary will begin to run.
        If a binary already exists on the host for this epoch height, the old one will be replaced.
        Otherwise a new binary will be added with this epoch height.
        ''')
    group.add_argument('--binary-idx',
                       type=int,
                       help='''
        0 based indexing.
        The index in the binary list that you want to replace.
        If the index does not exist on the host this operation will not do anything.
        ''')
    amend_binaries_parsers.set_defaults(func=amend_binaries_cmd)

    run_cmd_parser = subparsers.add_parser('run-cmd',
                                           help='''Run the cmd on the hosts.''')
    run_cmd_parser.add_argument('--cmd', type=str)
    run_cmd_parser.set_defaults(func=run_remote_cmd)

    env_cmd_parser = subparsers.add_parser(
        'env', help='''Update the environment variable on the hosts.''')
    env_cmd_parser.add_argument('--clear-all', action='store_true')
    env_cmd_parser.add_argument('--key-value', type=str, nargs='+')
    env_cmd_parser.set_defaults(func=run_env_cmd)


if __name__ == '__main__':
    parser = build_parser()
    args = parser.parse_args()
    ctx = CommandContext(args)
    args.func(ctx)
