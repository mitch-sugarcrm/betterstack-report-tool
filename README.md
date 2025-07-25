# BetterStack Uptime Calculator

## Overview

The BetterStack Uptime Calculator is a Rust-based command-line tool designed to calculate and report uptime statistics for BetterStack monitors over a specified date range. This tool leverages asynchronous programming to efficiently process multiple monitors concurrently, providing fast and accurate uptime calculations.

Originally created by Ron McCorkle for Pingdom (mack42 on GitHub): https://github.com/ZerosAndOnesLLC/pingdom-report-tool

Modified for BetterStack by Mitch Lewandowski for SugarCRM (mitch-sugarcrm on GitHub)

## Features

- Calculates uptime percentages and downtime minutes for all BetterStack monitors
- Supports custom date ranges for calculations
- Optional filtering to only include monitors published on status pages
- Utilizes asynchronous programming for improved performance
- Processes multiple monitors concurrently (up to 10 at a time)
- Reads BetterStack API credentials from environment variables or a .env file
- Provides a user-friendly command-line interface with usage instructions

## Prerequisites

- Rust programming language (latest stable version)
- BetterStack account with API access

## Setup

1. Clone the repository:
   ```sh
   git clone https://github.com/mitch-sugarcrm/betterstack-report-tool.git
   cd betterstack-report-tool
   ```

2. Set up your BetterStack API credentials:
   You have two options:

   a. Create a `.env` file in the project root:
      ```
      BETTERSTACK_API_TOKEN=your_api_token_here
      BETTERSTACK_API_URL=https://uptime.betterstack.com/api/v2
      ```

   b. Set environment variables directly in your shell:
      ```sh
      export BETTERSTACK_API_TOKEN=your_api_token_here
      export BETTERSTACK_API_URL=https://uptime.betterstack.com/api/v2
      ```
      NOTE: You can create an API token in your BetterStack account settings under the API section.

3. Build the project:
   ```sh
   cargo build --release
   ```

## Usage

After compiling, you can run the tool directly without using `cargo run`. The compiled binary will be in the `target/release` directory.

1. If you're in the project root, you can run:
   ```sh
   ./target/release/betterstack-report --start-date MM/DD/YYYY --end-date MM/DD/YYYY
   ```

2. Alternatively, you can move the binary to a directory in your PATH and run it from anywhere:
   ```sh
   betterstack-report --start-date MM/DD/YYYY --end-date MM/DD/YYYY
   ```

Examples:
```sh
# Calculate uptime for all monitors
betterstack-report --start-date 01/01/2024 --end-date 12/31/2024

# Calculate uptime only for monitors published on status pages
betterstack-report --start-date 01/01/2024 --end-date 12/31/2024 --status-page-only
```

This will calculate the uptime for your BetterStack monitors from January 1, 2024, to December 31, 2024. The `--status-page-only` flag is useful for generating reports that focus only on customer-facing monitors.

If you prefer to run it with cargo during development, you can still use:
```sh
cargo run -- --start-date MM/DD/YYYY --end-date MM/DD/YYYY
```

## Output

The tool will display the uptime statistics for each monitor in the following format:
```
Monitor Name, Uptime Percentage%, Downtime Minutes
```

## Notes

- The tool uses a small delay (200ms) between API requests to avoid rate limiting. Adjust this in the code if necessary.
- Ensure your BetterStack API token has the necessary permissions to access monitor information and SLA data.
- If you're using the `.env` file, make sure it's in the same directory as the binary when running the compiled version.

## Dependencies

- reqwest: HTTP client for making API requests
- serde and serde_json: For JSON serialization and deserialization
- chrono: For date and time handling
- tokio: Asynchronous runtime
- clap: For parsing command-line arguments
- dotenv: For loading environment variables from a .env file
- futures: For concurrent processing of API requests

## Contributing

Contributions to improve the BetterStack Uptime Calculator are welcome. Please feel free to submit issues or pull requests.

## License

This project is licensed under the MIT License with an additional clause restricting resale.

MIT License

Copyright (c) [2025] SugarCRM, Inc.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or use copies
of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

1. The above copyright notice and this permission notice shall be included in all
   copies or substantial portions of the Software.

2. The Software, or any modifications or derivative works based on the Software,
   shall not be resold or redistributed for a fee without explicit permission
   from the copyright holder.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
