from rc import pmap
import local_test_node
import node_config
import remote_node
from node_handle import NodeHandle
from utils import ScheduleContext

import numpy as np
import pathlib
import random
import json
import shutil
import re
import datetime
import sys
from typing import Optional

sys.path.append(str(pathlib.Path(__file__).resolve().parents[2] / 'lib'))

from configured_logger import logger


def to_list(item):
    return [item] if item is not None else []


class CommandContext:

    def __init__(self, args):
        self.args = args
        self.nodes = []
        self.traffic_generator = None
        self.schedule_ctx = self._make_schedule_context()

        # will set the schedule context in the nodes if the command is scheduled
        self._set_nodes()
        # Filter hosts based on the base args of the command
        self._filter_hosts()

    def is_scheduled(self) -> bool:
        return self.schedule_ctx is not None

    def get_targeted(self) -> list[NodeHandle]:
        """
        Returns the nodes and traffic generator with the previous schedule context.
        """
        return self.nodes + to_list(self.traffic_generator)

    def get_targeted_with_schedule_ctx(self) -> list[NodeHandle]:
        """
        Sets the schedule context in the nodes and traffic generator.
        All the commands will be scheduled if schedule is set.
        """
        return [
            node.with_schedule_ctx(self.schedule_ctx)
            for node in self.get_targeted()
        ]

    def get_without_schedule_ctx(self) -> list[NodeHandle]:
        """
        Clears the schedule context in the nodes and traffic generator.
        All the commands will be executed now.
        """
        return [node.with_schedule_ctx(None) for node in self.get_targeted()]

    def _set_nodes(self):
        """
        Get the nodes in the network.
        """
        if self.args.local_test:
            self._get_local_nodes()
        else:
            self._get_remote_nodes()

    def _get_local_nodes(self):
        """
        Get nodes in the local infrastructure.
        """
        if (self.args.chain_id is not None or
                self.args.start_height is not None or
                self.args.unique_id is not None or
                self.args.mocknet_id is not None):
            sys.exit(
                f'cannot give --chain-id, --start-height, --unique-id or --mocknet-id along with --local-test'
            )
            traffic_generator, nodes = local_test_node.get_nodes()
            node_config.configure_nodes(nodes + to_list(traffic_generator),
                                        node_config.TEST_CONFIG)
        self.traffic_generator = traffic_generator
        self.nodes = nodes

    def _get_remote_nodes(self):
        """
        Get nodes in the remote infrastructure.
        """
        if (self.args.chain_id is not None and
                self.args.start_height is not None and
                self.args.unique_id is not None):
            mocknet_id = self.args.chain_id + '-' + str(
                self.args.start_height) + '-' + self.args.unique_id
        elif self.args.mocknet_id is not None:
            mocknet_id = self.args.mocknet_id
        else:
            sys.exit(
                f'must give all of --chain-id --start-height and --unique-id or --mocknet-id'
            )
        traffic_generator, nodes = remote_node.get_nodes(mocknet_id)
        node_config.configure_nodes(nodes + to_list(traffic_generator),
                                    node_config.REMOTE_CONFIG)
        self.traffic_generator = traffic_generator
        self.nodes = nodes

    def _filter_hosts(self):
        """
        Select the affected hosts.
        traffic_generator can become None,
        nodes list can become empty
        """
        # Only keep nodes that want a neard runner not the auxiliary nodes i.e. tracers.
        self.nodes = [node for node in self.nodes if node.want_neard_runner]

        if self.args.host_filter is not None and self.traffic_generator is not None:
            if not re.search(self.args.host_filter,
                             self.traffic_generator.name()):
                self.traffic_generator = None
            self.nodes = [
                h for h in self.nodes
                if re.search(self.args.host_filter, h.name())
            ]
        if self.args.host_type not in ['all', 'traffic']:
            self.traffic_generator = None
        if self.args.host_type not in ['all', 'nodes']:
            self.nodes = []

        if len(self.nodes) == 0 and self.traffic_generator == None:
            logger.error(f'No hosts selected. Change filters and try again.')
            exit(1)

        if self.args.select_partition is not None:
            i, n = self.args.select_partition

            if len(self.nodes) < n and self.traffic_generator == None:
                logger.error(
                    f'Partitioning {len(self.nodes)} nodes in {n} groups will result in empty groups.'
                )
                exit(1)
            self.nodes.sort(key=lambda node: node.name())
            self.nodes = np.array_split(self.nodes, n)[i - 1]

    def _make_schedule_context(self) -> Optional[ScheduleContext]:
        """
        Make a schedule context if the command is scheduled.
        """
        if getattr(self.args, 'schedule_in', None) is None:
            return None

        context = ScheduleContext(
            id=getattr(self.args, 'schedule_id', None),
            time_spec=self.args.schedule_in,
        )
        return context


def prompt_init_flags(args):
    if args.neard_binary_url is None:
        print('neard binary URL?: ')
        args.neard_binary_url = sys.stdin.readline().strip()
        assert len(args.neard_binary_url) > 0

    if args.neard_upgrade_binary_url == "":
        args.neard_upgrade_binary_url = None
        return

    if args.neard_upgrade_binary_url is None:
        print(
            'add a second neard binary URL to upgrade to mid-test? enter nothing here to skip: '
        )
        url = sys.stdin.readline().strip()
        if len(url) > 0:
            args.neard_upgrade_binary_url = url


def init_neard_runners(ctx: CommandContext, remove_home_dir=False):
    args = ctx.args
    nodes = ctx.nodes
    traffic_generator = ctx.traffic_generator
    prompt_init_flags(args)
    if args.neard_upgrade_binary_url is None or args.neard_upgrade_binary_url == '':
        configs = [{
            "is_traffic_generator": False,
            "binaries": [{
                "url": args.neard_binary_url,
                "epoch_height": 0
            }]
        }] * len(nodes)
        traffic_generator_config = {
            "is_traffic_generator": True,
            "binaries": [{
                "url": args.neard_binary_url,
                "epoch_height": 0
            }]
        }
    else:
        # for now this test starts all validators with the same stake, so just make the upgrade
        # epoch random. If we change the stakes, we should change this to choose how much stake
        # we want to upgrade during each epoch
        configs = []
        for i in range(len(nodes)):
            configs.append({
                "is_traffic_generator":
                    False,
                "binaries": [{
                    "url": args.neard_binary_url,
                    "epoch_height": 0
                }, {
                    "url": args.neard_upgrade_binary_url,
                    "epoch_height": random.randint(1, 4)
                }]
            })
        traffic_generator_config = {
            "is_traffic_generator":
                True,
            "binaries": [{
                "url": args.neard_upgrade_binary_url,
                "epoch_height": 0
            }]
        }

    if traffic_generator is not None:
        traffic_generator.init_neard_runner(traffic_generator_config,
                                            remove_home_dir)
    pmap(lambda x: x[0].init_neard_runner(x[1], remove_home_dir),
         zip(nodes, configs))


def init_cmd(ctx: CommandContext):
    init_neard_runners(ctx, remove_home_dir=False)


def update_binaries_cmd(ctx: CommandContext):
    pmap(lambda node: node.neard_runner_update_binaries(),
         ctx.get_targeted_with_schedule_ctx())


def prompt_setup_flags(args, dumper_node_names):
    if not args.yes:
        print(
            'this will reset all nodes\' home dirs and initialize them with new state. continue? [yes/no]'
        )
        if sys.stdin.readline().strip() != 'yes':
            sys.exit()

    if not args.gcs_state_sync and len(dumper_node_names) > 0:
        print(
            f'--gcs-state-sync not provided, but there are state dumper nodes: {dumper_node_names}. continue with dumper nodes as normal RPC nodes? [yes/no]'
        )
        if sys.stdin.readline().strip() != 'yes':
            sys.exit()

    if args.epoch_length is None:
        print('epoch length for the initialized genesis file?: ')
        args.epoch_length = int(sys.stdin.readline().strip())

    if args.num_validators is None:
        print('number of validators?: ')
        args.num_validators = int(sys.stdin.readline().strip())

    if args.num_seats is None:
        args.num_seats = args.num_validators


# Only print stdout and stderr if they are not empty
def print_result(node, result):
    RED = '\033[91m'
    GREEN = '\033[92m'
    RESET = '\033[0m'
    stdout = f'\n{GREEN}stdout:{RESET}\n{result.stdout}' if result.stdout != "" else ''
    stderr = f'\n{RED}stderr:{RESET}\n{result.stderr}' if result.stderr != "" else ''
    logger.info('{0}:{1}{2}'.format(node.name(), stdout, stderr))


def _run_remote(hosts, cmd):
    pmap(
        lambda node: print_result(node, node.run_cmd(cmd, return_on_fail=True)),
        hosts,
        on_exception="")


def run_remote_cmd(ctx: CommandContext):
    targeted = ctx.get_targeted_with_schedule_ctx()
    logger.info(f'Running cmd on {",".join([h.name() for h in targeted])}')
    _run_remote(targeted, ctx.args.cmd)


def run_remote_upload_file(ctx: CommandContext):
    targeted = ctx.get_targeted()
    logger.info(
        f'Uploading {ctx.args.src} in {ctx.args.dst} on {",".join([h.name() for h in targeted])}'
    )
    pmap(lambda node: print_result(
        node, node.upload_file(ctx.args.src, ctx.args.dst)),
         targeted,
         on_exception="")


def run_env_cmd(ctx: CommandContext):
    if ctx.args.clear_all:
        func = lambda node: node.neard_clear_env()
    else:
        func = lambda node: node.neard_update_env(ctx.args.key_value)
    pmap(func, ctx.get_targeted_with_schedule_ctx())


def list_scheduled_cmds(ctx: CommandContext):
    targeted = ctx.get_targeted()
    cmd = 'systemctl --user --legend=false list-timers "mocknet*" --all'
    if ctx.args.full:
        cmd += '; systemctl --user show "mocknet-*" -p Id -p Description --value'
    logger.info(
        f'Getting schedule from {",".join([h.name() for h in targeted])}')
    _run_remote(targeted, cmd)


def clear_scheduled_cmds(ctx: CommandContext):
    targeted = ctx.get_targeted()
    filter = ctx.args.filter
    if not filter.startswith('mocknet-'):
        filter = 'mocknet-' + filter
    logger.info(
        f'Clearing scheduled commands matching "{filter}" from {",".join([h.name() for h in targeted])}'
    )
    cmd = f'systemctl --user stop "{filter}"'
    _run_remote(targeted, cmd)


def new_genesis_timestamp(node):
    version = node.neard_runner_version()
    err = version.get('error')
    if err is not None:
        if err['code'] != -32601:
            sys.exit(
                f'bad response calling version RPC on {node.name()}: {err}')
        return None
    genesis_time = None
    result = version.get('result')
    if result is not None:
        if result.get('node_setup_version') == '1':
            genesis_time = str(datetime.datetime.now(tz=datetime.timezone.utc))
    return genesis_time


# returns boot nodes and validators we want for the new test network
def get_network_nodes(new_test_rpc_responses, num_validators):
    validators = []
    non_validators = []
    boot_nodes = []
    for node, response in new_test_rpc_responses:
        if len(validators) < num_validators:
            if node.can_validate and response[
                    'validator_account_id'] is not None:
                # we assume here that validator_account_id is not null, validator_public_key
                # better not be null either
                validators.append({
                    'account_id': response['validator_account_id'],
                    'public_key': response['validator_public_key'],
                    'amount': str(10**33),
                })
        else:
            non_validators.append(node.ip_addr())
        if len(boot_nodes) < 20:
            boot_nodes.append(
                f'{response["node_key"]}@{node.ip_addr()}:{response["listen_port"]}'
            )

        if len(validators) >= num_validators and len(boot_nodes) >= 20:
            break
    # neither of these should happen, since we check the number of available nodes in new_test(), and
    # only the traffic generator will respond with null validator_account_id and validator_public_key
    if len(validators) == 0:
        sys.exit('no validators available after new_test RPCs')
    if len(validators) < num_validators:
        logger.warning(
            f'wanted {num_validators} validators, but only {len(validators)} available'
        )
    return validators, boot_nodes


def _apply_stateless_config(args, node):
    """Applies configuration changes to the node for stateless validation,
    including changing config.json file and updating TCP buffer size at OS level."""
    # TODO: it should be possible to update multiple keys in one RPC call so we dont have to make multiple round trips
    # TODO: Enable saving witness after fixing the performance problems.
    do_update_config(node, 'save_latest_witnesses=false')
    if not node.want_state_dump:
        do_update_config(node, 'tracked_shards_config="NoShards"')
        do_update_config(node, 'store.load_mem_tries_for_tracked_shards=true')
    if not args.local_test:
        node.run_cmd(
            "sudo sysctl -w net.core.rmem_max=8388608 && sudo sysctl -w net.core.wmem_max=8388608 && sudo sysctl -w net.ipv4.tcp_rmem='4096 87380 8388608' && sudo sysctl -w net.ipv4.tcp_wmem='4096 16384 8388608' && sudo sysctl -w net.ipv4.tcp_slow_start_after_idle=0"
        )


def _apply_config_changes(node, state_sync_location):
    if state_sync_location is None:
        changes = {'state_sync_enabled': False}
    else:
        changes = {
            'state_sync.sync': {
                'ExternalStorage': {
                    'location': state_sync_location
                }
            }
        }
        if node.want_state_dump:
            changes['state_sync.dump.location'] = state_sync_location
            # TODO: Change this to Enabled once we remove support the for EveryEpoch alias.
            changes[
                'store.state_snapshot_config.state_snapshot_type'] = "EveryEpoch"
    for key, change in changes.items():
        do_update_config(node, f'{key}={json.dumps(change)}')


def _clear_state_parts_if_exists(location, nodes):
    # TODO: Maybe add an argument to set the epoch height from where we want to cleanup.
    # It still works without it because the dumper node will start dumping the current epoch after reset.

    if location is None:
        return

    state_dumper_node = next(filter(lambda n: n.want_state_dump, nodes), None)
    if state_dumper_node is None:
        logger.info('No state dumper node found, skipping state parts cleanup.')
        return
    logger.info('State dumper node found, cleaning up state parts.')

    if location.get('Filesystem') is not None:
        root_dir = location['Filesystem']['root_dir']
        shutil.rmtree(root_dir)
        return

    # For GCS-based state sync, looks for the state dumper and clears the GCP
    # bucket where it dumped the parts.
    bucket_name = location['GCS']['bucket']

    state_dumper_node.run_cmd(f'gsutil -m rm -r gs://{bucket_name}/chain_id=*',
                              return_on_fail=True)


def _get_state_parts_bucket_name(args):
    return f'near-state-dumper-mocknet-{args.chain_id}-{args.start_height}-{args.unique_id}'


def _get_state_parts_location(args):
    if args.local_test:
        return {
            "Filesystem": {
                "root_dir":
                    str(local_test_node.DEFAULT_LOCAL_MOCKNET_DIR /
                        'state-parts')
            }
        }
    return {"GCS": {"bucket": _get_state_parts_bucket_name(args)}}


def new_test_cmd(ctx: CommandContext):
    args = ctx.args
    nodes = ctx.nodes
    traffic_generator = ctx.traffic_generator
    prompt_setup_flags(args, [n.name() for n in nodes if n.want_state_dump])

    if args.epoch_length <= 0:
        sys.exit(f'--epoch-length should be positive')
    if args.num_validators <= 0:
        sys.exit(f'--num-validators should be positive')
    if len(nodes) < args.num_validators:
        sys.exit(
            f'--num-validators is {args.num_validators} but only found {len(nodes)} under test'
        )

    ref_node = traffic_generator if traffic_generator else nodes[0]
    genesis_time = new_genesis_timestamp(ref_node)

    targeted = nodes + to_list(traffic_generator)

    logger.info(f'resetting/initializing home dirs')
    test_keys = pmap(lambda node: node.neard_runner_new_test(), targeted)

    validators, boot_nodes = get_network_nodes(zip(nodes, test_keys),
                                               args.num_validators)
    logger.info("""Setting validators: {0}
Run `status` to check if the nodes are ready. After they're ready,
 you can run `start-nodes` and `start-traffic`""".format(validators))

    pmap(
        lambda node: node.neard_runner_network_init(
            validators,
            boot_nodes,
            args.state_source,
            args.patches_path,
            args.epoch_length,
            args.num_seats,
            args.new_chain_id,
            args.genesis_protocol_version,
            genesis_time=genesis_time,
        ), targeted)

    location = None
    if args.gcs_state_sync:
        location = _get_state_parts_location(args)
    logger.info('Applying default config changes')
    pmap(lambda node: _apply_config_changes(node, location), targeted)
    if args.stateless_setup:
        logger.info('Configuring nodes for stateless protocol')
        pmap(lambda node: _apply_stateless_config(args, node), nodes)

    _clear_state_parts_if_exists(location, nodes)


def do_update_config(node, config_change):
    result = node.neard_update_config(config_change)
    if not result:
        logger.warning(
            f'failed updating config on {node.name()}. result: {result}')


def status_cmd(ctx: CommandContext):
    targeted = ctx.get_targeted()
    statuses = pmap(lambda node: node.neard_runner_ready(), targeted)

    not_ready = []
    for ready, node in zip(statuses, targeted):
        if not ready:
            not_ready.append(node.name())

    if len(not_ready) == 0:
        print(f'all {len(targeted)} nodes ready')
    else:
        print(
            f'{len(targeted)-len(not_ready)}/{len(targeted)} ready. Nodes not ready: {not_ready[:3]}'
        )


def reset_cmd(ctx: CommandContext):
    args = ctx.args
    nodes = ctx.nodes
    traffic_generator = ctx.traffic_generator

    if not args.yes:
        print(
            'this will reset selected nodes\' home dirs to their initial states right after test initialization finished. continue? [yes/no]'
        )
        if sys.stdin.readline().strip() != 'yes':
            sys.exit()
    if args.backup_id is None:
        ref_node = traffic_generator if traffic_generator else nodes[0]
        backups = ref_node.neard_runner_ls_backups()
        backups_msg = 'ID |  Time  | Description\n'
        if 'start' not in backups:
            backups_msg += 'start | None | initial test state after state root computation\n'
        for backup_id, backup_data in backups.items():
            backups_msg += f'{backup_id} | {backup_data.get("time")} | {backup_data.get("description")}\n'

        print(f'Backups as reported by {ref_node.name()}):\n\n{backups_msg}')
        print('please enter a backup ID here:')
        args.backup_id = sys.stdin.readline().strip()
        if args.backup_id != 'start' and args.backup_id not in backups:
            print(
                f'Given backup ID ({args.backup_id}) was not in the list given')
            sys.exit()

    pmap(lambda node: node.neard_runner_reset(backup_id=args.backup_id),
         ctx.get_targeted_with_schedule_ctx())
    logger.info(
        'Data dir reset in progress. Run the `status` command to see when this is finished. Until it is finished, neard runners may not respond to HTTP requests.'
    )
    # Do not clear state parts if scheduling
    # NOTE: This may be a problem if you want to schedule the reset command on a dumper node
    #       because the dumper node will start dumping the current epoch after reset.
    if not ctx.is_scheduled():
        _clear_state_parts_if_exists(_get_state_parts_location(args), nodes)


def update_config_cmd(ctx: CommandContext):
    pmap(
        lambda node: do_update_config(node, ctx.args.set),
        ctx.get_targeted_with_schedule_ctx(),
    )


def start_nodes_cmd(ctx: CommandContext):
    nodes = ctx.nodes
    if not all(pmap(lambda node: node.neard_runner_ready(), nodes)):
        logger.warning(
            'not all nodes are ready to start yet. Run the `status` command to check their statuses'
        )
        return
    pmap(
        lambda node: node.with_schedule_ctx(ctx.schedule_ctx).
        neard_runner_start(), nodes)
    # Wait for the nodes to be up if not scheduling
    if not ctx.is_scheduled():
        pmap(lambda node: node.wait_node_up(), nodes)


def stop_nodes_cmd(ctx: CommandContext):
    pmap(lambda node: node.neard_runner_stop(),
         ctx.get_targeted_with_schedule_ctx())


def hard_reset_cmd(ctx: CommandContext):
    print("""
        WARNING!!!!
        WARNING!!!!
        This will undo all chain state, which will force a restart from the beginning,
        including the genesis state computation which takes several hours.
        Continue? [yes/no]""")
    if sys.stdin.readline().strip() != 'yes':
        return
    init_neard_runners(ctx, remove_home_dir=True)
    _clear_state_parts_if_exists(_get_state_parts_location(args), nodes)
