# KaspaTalk backend

Run the backend on the same machine where you're running your rusty-kaspa node, configured with Testnet11 (use the last version rusty-kaspa).

## Activate the backend from source code (on Linux Ubuntu/Mint)
- Run your rusty-kaspa node on testnet-11, with these parameters:
	- ./kaspad --testnet --netsuffix=11 --utxoindex --rpclisten-borsh=0.0.0.0:17120
- Open your terminal
- Git clone this repository
- Open tha main folder and run the following command:
	- cargo build --release
- After completing the compilation, run the following commands:
	- cd /target/release
 	- ./kaspatalk-backend --rusty-kaspa-address=localhost:17120
- If connection to rusty-kaspa works, you'll see a message starting with: "Server info: GetServerInfoResponse..."
- If you need to run the frontend on a different machine, make sure port 3000 is open, to allow the frontend to connect to the backend
 
