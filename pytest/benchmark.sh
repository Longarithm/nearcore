#!/bin/bash

# Configuration
LOG_FILE="/tmp/err"
SCRIPT_MODE=""
OPERATION=""
NODE_RPC="http://127.0.0.1:3030"
NONCE=""
VALIDATOR_IPS_FILE=""
USE_RANDOM_VALIDATORS=false

# Function to show usage
usage() {
    echo "Usage: $0 [--mode 1|2] [--operation prepare|run|stop] [--rpc-url URL] [--nonce NUMBER] [--validator-ips-file FILE] [--use-random-validators]"
    echo "  --mode: Select benchmark mode (1 for native transfers, 2 for sweat benchmark)"
    echo "  --operation: Select operation type (prepare, run, or stop)"
    echo "  --rpc-url: RPC URL (optional, defaults to http://127.0.0.1:3030)"
    echo "  --nonce: Starting nonce for prepare mode (optional)"
    echo "  --validator-ips-file: Path to file containing validator IPs (optional)"
    echo "  --use-random-validators: Send transactions to random validators (optional)"
    exit 1
}

# Parse command line arguments
while [[ $# -gt 0 ]]; do
    case $1 in
        --mode)
            MODE="$2"
            shift 2
            ;;
        --operation)
            OPERATION="$2"
            shift 2
            ;;
        --rpc-url)
            NODE_RPC="$2"
            shift 2
            ;;
        --nonce)
            NONCE="$2"
            shift 2
            ;;
        --validator-ips-file)
            VALIDATOR_IPS_FILE="$2"
            shift 2
            ;;
        --use-random-validators)
            USE_RANDOM_VALIDATORS=true
            shift
            ;;
        *)
            usage
            ;;
    esac
done

# Get network name from environment variable or use default
if [ -z "$UNIQUE_ID" ]; then
    NETWORK_NAME="local"
else
    NETWORK_NAME="$UNIQUE_ID"
fi

# Validate inputs
if [[ -n "$MODE" && ! "$MODE" =~ ^(1|2)$ ]]; then
    echo "Error: Mode must be 1 or 2"
    usage
fi

if [[ -n "$OPERATION" && ! "$OPERATION" =~ ^(prepare|run|stop)$ ]]; then
    echo "Error: Operation must be prepare, run, or stop"
    usage
fi

if [[ "$OPERATION" == "prepare" && -n "$NONCE" && ! "$NONCE" =~ ^[0-9]+$ ]]; then
    echo "Error: Nonce must be a number"
    usage
fi

# Check if validator IPs file exists when using random validators
if [[ "$USE_RANDOM_VALIDATORS" == true && -n "$VALIDATOR_IPS_FILE" && ! -f "$VALIDATOR_IPS_FILE" ]]; then
    echo "Error: Validator IPs file not found: $VALIDATOR_IPS_FILE"
    exit 1
fi

# Function to run command in background with logging
run_background() {
    local cmd="$1"
    local log_file="$2"
    local mode_suffix="$3"
    echo "Running command in background: $cmd"
    eval "$cmd" > "$log_file" 2>&1 &
    local pid=$!
    echo "Process ID: $pid"
    echo "Logs are being written to $log_file"
    echo $pid > "/tmp/benchmark_pid_${NETWORK_NAME}_${mode_suffix}"
}

# Function to check status
check_status() {
    local pid=$1
    local log_file=$2
    echo "=== Checking status of background process $pid ==="
    if ! ps -p $pid > /dev/null; then
        echo "Process $pid has finished"
        echo "Last 10 lines of log:"
        tail -n 10 "$log_file"
        return 1
    else
        echo "Process $pid is still running"
        echo "Last 5 lines of log:"
        tail -n 5 "$log_file"
        return 0
    fi
}

# Function to stop benchmark processes
stop_benchmark() {
    echo "Stopping near-synth-bm processes..."
    
    # Kill all near-synth-bm processes
    if pgrep -f "near-synth-bm" > /dev/null; then
        pkill -f "near-synth-bm"
        echo "Stopped all near-synth-bm processes"
    else
        echo "No near-synth-bm processes found"
    fi
    
    # Clean up PID files
    rm -f /tmp/benchmark_pid_${NETWORK_NAME}_mode*
    
    # Clean up log file
    if [ -f "$LOG_FILE" ]; then
        echo "Benchmark stopped at $(date)" >> "$LOG_FILE"
    fi
    
    echo "Benchmark stopped successfully"
}

# Function to prepare RPC URLs for random validators
prepare_rpc_urls() {
    if [[ "$USE_RANDOM_VALIDATORS" == true && -n "$VALIDATOR_IPS_FILE" && -f "$VALIDATOR_IPS_FILE" ]]; then
        echo "Preparing RPC URLs from validator IPs file: $VALIDATOR_IPS_FILE"
        
        # Create a temporary file to store RPC URLs
        local rpc_urls_file="/tmp/rpc_urls_${NETWORK_NAME}"
        > "$rpc_urls_file"  # Clear file if it exists
        
        # Extract IPs from validator IPs file and create RPC URLs
        grep -v "^#" "$VALIDATOR_IPS_FILE" | while read -r line; do
            local validator_ip=$(echo "$line" | cut -d'|' -f3)
            if [ -n "$validator_ip" ]; then
                echo "http://${validator_ip}:3030" >> "$rpc_urls_file"
            fi
        done
        
        local url_count=$(wc -l < "$rpc_urls_file")
        echo "Created $url_count RPC URLs for random selection"
        
        # Return the path to the RPC URLs file
        echo "$rpc_urls_file"
    else
        # If not using random validators, return empty string
        echo ""
    fi
}

# Mode 1: Native Transfers
run_mode_1() {
    local operation=$1
    local rpc_url=$2
    local log_file="/tmp/err"
    local nonce_arg=""
    
    if [ -n "$NONCE" ]; then
        nonce_arg="--nonce $NONCE"
    else
        nonce_arg="--nonce 10"
    fi
    
    # Prepare RPC URLs for random validators if needed
    # local rpc_urls_file=""
    # if [ "$operation" == "run" ] && [ "$USE_RANDOM_VALIDATORS" == true ]; then
    #     rpc_urls_file=$(prepare_rpc_urls)
    #     echo "Using RPC URLs from file: $rpc_urls_file"
    # fi
    
    if [ "$operation" == "prepare" ]; then
        run_background "RUST_LOG='hyper_util=info,debug' /home/ubuntu/near-synth-bm create-sub-accounts \
            --rpc-url $rpc_url \
            --signer-key-path /home/ubuntu/.near/validator_key.json \
            $nonce_arg \
            --sub-account-prefixes '2,c,f,h,m,x' \
            --num-sub-accounts 600 \
            --deposit 953060601875000000010000000 \
            --channel-buffer-size 1200 \
            --requests-per-second 1250 \
            --user-data-dir user-data && \
            echo 'Mode 1 preparation complete'" "$log_file" "mode1"
    else
        local random_validators_arg=""
        if [ -n "/tmp/validator_ips" ]; then
            random_validators_arg="--rpc-urls-file /tmp/validator_ips"
        fi
        
        # --num-transfers 90000000
        run_background "RUST_LOG=info /home/ubuntu/near-synth-bm benchmark-native-transfers \
            --rpc-url $rpc_url \
            $random_validators_arg \
            --user-data-dir /home/ubuntu/user-data/ \
            --read-nonces-from-network \
            --channel-buffer-size 30000 \
            --requests-per-second 4000 \
            --amount 1" "$log_file" "mode1"
    fi
}

# Mode 2: Sweat Benchmark
run_mode_2() {
    local operation=$1
    local rpc_url=$2
    local log_file="/tmp/err"
    local nonce_arg=""
    
    if [ -n "$NONCE" ]; then
        nonce_arg="--nonce $NONCE"
    else
        nonce_arg="--nonce 10"
    fi
    
    # Prepare RPC URLs for random validators if needed
    # local rpc_urls_file=""
    # if [ "$operation" == "run" ] && [ "$USE_RANDOM_VALIDATORS" == true ]; then
    #     rpc_urls_file=$(prepare_rpc_urls)
    #     echo "Using RPC URLs from file: $rpc_urls_file"
    # fi
    
    if [ "$operation" == "prepare" ]; then
        run_background "RUST_LOG='hyper_util=info,debug' /home/ubuntu/near-synth-bm \
            benchmark-sweat create-contracts \
            --rpc-url $rpc_url \
            --num-oracles 6 \
            --oracle-deposit 1000000000000000000000000000000 \
            --user-data-dir /home/ubuntu/oracles/ \
            --signer-key-path /home/ubuntu/.near/validator_key.json \
            --wasm-file /home/ubuntu/sweat.wasm \
            --sub-account-prefixes '2,c,f,h,m,x' \
            $nonce_arg && \
        RUST_LOG='near_jsonrpc_client=info,hyper_util=info,debug' /home/ubuntu/near-synth-bm \
            benchmark-sweat create-users \
            --rpc-url $rpc_url \
            --oracle-data-dir /home/ubuntu/oracles/ \
            --users-per-oracle 1000 \
            --user-data-dir /home/ubuntu/users/ \
            --deposit 953060601875000000010000 && \
            echo 'Mode 2 preparation complete'" "$log_file" "mode2"
    else
        local random_validators_arg=""
        if [ -n "/tmp/validator_ips" ]; then
            random_validators_arg="--rpc-urls-file /tmp/validator_ips"
        fi
        
        # --total-batches 20000000
        run_background "RUST_LOG=info /home/ubuntu/near-synth-bm \
            benchmark-sweat run-benchmark \
            --rpc-url $rpc_url \
            $random_validators_arg \
            --oracle-data-dir /home/ubuntu/oracles/ \
            --user-data-dir /home/ubuntu/users/ \
            --batch-size 750 \
            --requests-per-second 15" "$log_file" "mode2"
    fi
}

# Run the specified modes
if [ "$OPERATION" == "stop" ]; then
    stop_benchmark
elif [ -n "$MODE" ]; then
    if [ "$MODE" == "1" ]; then
        run_mode_1 "$OPERATION" "$NODE_RPC"
    elif [ "$MODE" == "2" ]; then
        run_mode_2 "$OPERATION" "$NODE_RPC"
    fi
    
    echo "Script started in background. To check status:"
    echo "  ps aux | grep near-synth-bm"
    echo "To view logs:"
    echo "  tail -f /tmp/err"
else
    # Only show usage if not stopping and no mode specified
    if [ "$OPERATION" != "stop" ]; then
        echo "Error: Mode must be specified for prepare or run operations"
        usage
    fi
fi