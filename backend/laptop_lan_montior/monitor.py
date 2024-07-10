from typing import Literal
import aiohttp
import asyncio
import math
import os
from pprint import pprint
from dotenv import load_dotenv

import click

load_dotenv()

wattmeter_api = os.environ.get("WATTMETER_API_URL", "http://127.0.0.1:4000/amps") 
tessie_api = os.environ.get("TESSIE_API_URL", "https://api.tessie.com/")
vehicle_vin = os.environ.get("VEHICLE_VIN", "")
tessie_token = os.environ.get("TESSIE_TOKEN", "")


async def tessie_request(method: Literal["GET"] | Literal["POST"], action: str, params: dict[str, str]):
    async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=10)) as session:
        async with session.request(method, tessie_api + action.format(vin=vehicle_vin), allow_redirects=True, params=params, headers={
            "accept": "application/json",
            "authorization": "Bearer " + tessie_token,
        }) as response:
            return await response.json()

async def get_current_car_amps():
    car_state = await tessie_request("GET", "{vin}/state", {})
    return car_state["charge_state"]["charge_amps"], car_state["charge_state"]["charge_current_request"]

async def get_amps():
    async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=5)) as session:
        async with session.get(wattmeter_api) as response:
            return float(await response.text())


async def set_car_charge_amps(requested_amps: int):
    await tessie_request("POST", f"{{vin}}/command/set_charging_amps?amps={requested_amps}", {})



async def set_amps_as_required(threshold: float, every_seconds: int, threshold_count: int, max_car_amps: int):
    current_overshoots = 0
    current_undershoots = 0
    current_car_amps, current_requested_car_amps = await get_current_car_amps()
    while True:
        amps = await get_amps()
        if amps > threshold:
            current_overshoots += 1
            current_undershoots = 0
            if current_overshoots >= threshold_count:
                safe_car_amps = min(max_car_amps, max(0, math.floor(threshold - amps + current_car_amps - 2)))
                if current_requested_car_amps > safe_car_amps:
                    # Now we need to undershoot!
                    safe_car_amps = min(max_car_amps, max(0, math.floor(threshold - amps + current_car_amps - 2)))
                    await set_car_charge_amps(safe_car_amps)
                    current_requested_car_amps = safe_car_amps
                elif current_requested_car_amps == safe_car_amps:
                    print(f">> Already asking for safe car amps: {safe_car_amps} (threshold={threshold}, amps={amps}) ")
                    current_car_amps, current_requested_car_amps = await get_current_car_amps()
                    continue
                print(f">> Asking for safe car amps: {safe_car_amps} (threshold={threshold}, amps={amps}) ")
                await set_car_charge_amps(safe_car_amps)
                current_requested_car_amps = safe_car_amps
                await asyncio.sleep(5)
                continue
        else:
            current_overshoots = 0
        if amps + 3 < threshold and current_car_amps == current_requested_car_amps:
            current_undershoots += 1
            if current_undershoots >= threshold_count:
                safe_car_amps = min(max_car_amps, max(0, math.floor(threshold - amps + current_car_amps - 2)))
                if safe_car_amps == current_car_amps:
                    print("We should increase car amps, but hit threshold limit. Ignoring.")
                else:
                    print(f"Should increase car amps to {safe_car_amps} (current={current_car_amps})")
                    await set_car_charge_amps(safe_car_amps)
                    current_car_amps, current_requested_car_amps = await get_current_car_amps()
                    await asyncio.sleep(5)
                    continue
        
        external_amps = amps - current_car_amps
        print(f"Everything is Milhouse (charging at {current_car_amps}A, external {external_amps}A, threshold={threshold}, amps={amps})")

        await asyncio.sleep(every_seconds)


async def main_loop(threshold: float, every_seconds: int, warn_after_threshold: int):
    current_warnings = 0
    while True:
        amps = await get_amps()
        print(amps)
        if amps > threshold:
            print(f">>> High current detected: {amps}")
            current_warnings += 1
            if current_warnings >= warn_after_threshold:
                print(f">>> Sending notification after {warn_after_threshold} warnings: {amps}")
                # Print a message through applescript
                os.system(f"osascript -e 'display notification \"{amps}\" with title \"High current detected\"'")
                # Use a system dialog alert instead of a notification
                # os.system(f"osascript -e 'display dialog \"High current detected: {amps}\" with title \"High current detected\"'")
        else:
            current_warnings = 0
        await asyncio.sleep(every_seconds)



@click.command()
@click.option("--every_seconds", type=int, default=1)
@click.option("--threshold", type=float, default=10.2)
@click.option("--warn_after_threshold", type=int, default=2)
@click.option("--max_car_amps", type=int, default=3)
def cli(threshold, every_seconds, warn_after_threshold, max_car_amps):
    # Do this in asyncio
    # Create a loop
    loop = asyncio.get_event_loop()
    loop.create_task(set_amps_as_required(threshold=threshold, every_seconds=every_seconds, threshold_count=warn_after_threshold, max_car_amps=max_car_amps))
    loop.create_task(main_loop(threshold=threshold, every_seconds=every_seconds, warn_after_threshold=warn_after_threshold))
    loop.run_forever()



if __name__ == "__main__":
    cli()
