from __future__ import annotations

import asyncio
import backoff
import json
import logging
import math
import os
import pickle
from collections.abc import AsyncGenerator
from contextlib import asynccontextmanager
from datetime import datetime, timedelta
from typing import Literal, TypedDict

import aiohttp
import asyncclick as click
from dotenv import load_dotenv

load_dotenv()

logger = logging.getLogger(__name__)
logger.setLevel(logging.DEBUG)

# Add a handler that will log messages with timestamp in front
log_format = "%(asctime)s - %(name)s - %(levelname)s - %(message)s"
handler = logging.StreamHandler()
handler.setFormatter(logging.Formatter(log_format))
logger.addHandler(handler)


wattmeter_api = os.environ.get("WATTMETER_API_URL", "http://127.0.0.1:4000/amps")
tessie_api = os.environ.get("TESSIE_API_URL", "https://api.tessie.com/")
vehicle_vin = os.environ.get("VEHICLE_VIN", "")
tessie_token = os.environ.get("TESSIE_TOKEN", "")
charger_latlon = os.environ.get("CHARGER_LATLON", "0.0,0.0")
charger_radius = float(os.environ.get("CHARGER_RADIUS", "150"))  # in meters


class GeoLocation(TypedDict):
    lat: float
    lon: float


def get_distance(a: GeoLocation, b: GeoLocation) -> float:
    """
    Calculate the distance between two GPS coordinates.

    Returns the distance in meters.
    """
    # https://en.wikipedia.org/wiki/Haversine_formula
    earth_radius = 6371000
    phi1 = math.radians(a["lat"])
    phi2 = math.radians(b["lat"])
    delta_phi = math.radians(b["lat"] - a["lat"])
    delta_lambda = math.radians(b["lon"] - a["lon"])
    a = (math.sin(delta_phi / 2) * math.sin(delta_phi / 2)) + (
        math.cos(phi1)
        * math.cos(phi2)
        * math.sin(delta_lambda / 2)
        * math.sin(delta_lambda / 2)
    )
    c = 2 * math.atan2(math.sqrt(a), math.sqrt(1 - a))
    return earth_radius * c


charger_lat, charger_lon = [float(x) for x in charger_latlon.split(",")]

charger_geo = GeoLocation(lat=charger_lat, lon=charger_lon)


class TessieChargeState(TypedDict):
    charge_amps: float
    charge_current_request: float
    charge_enable_request: bool
    charge_energy_added: float
    charge_limit_soc: int
    charge_limit_soc_max: int
    charge_limit_soc_min: int
    charge_limit_soc_std: int
    charge_miles_added_ideal: float
    charge_miles_added_rated: float
    charge_port_cold_weather_mode: bool
    charge_port_door_open: bool
    charge_port_latch: str
    charge_rate: float
    charger_actual_current: float
    charger_phases: Literal[1, 3] | None
    charger_pilot_current: float
    charger_power: float
    charger_voltage: float
    charging_state: Literal[
        "Complete",
        "Charging",
        "Disconnected",
        "Pending",
        "Starting",
        "Stopped",
    ]
    conn_charge_cable: str | Literal["IEC", "J1772", "CCS", "SCHUKO", "UNKNOWN"]
    fast_charger_brand: str
    fast_charger_present: bool


class TessieDriveState(TypedDict):
    gps_as_of: int
    latitude: float
    longitude: float
    heading: int
    speed: int


class TessieCarState(TypedDict):
    access_type: str
    api_version: int
    display_name: str
    drive_state: TessieDriveState
    charge_state: TessieChargeState


class ChargingState:
    def __init__(
        self,
        aiohttp_session: aiohttp.ClientSession,
        max_house_amps: float = 16,
        max_car_amps: float = 16,
        charger_geo: GeoLocation = charger_geo,
        charger_radius: float = charger_radius,
        is_enabled: bool = True,
    ):
        self.aiohttp_session = aiohttp_session
        self.api_cache_time = timedelta(seconds=30)
        self.wattmeter_sliding_window_size = timedelta(minutes=5)
        self.wattmeter_sliding_window_resolution = timedelta(seconds=1)
        self.wattmeter_sliding_window = []
        self.car_amps_sliding_window = []

        self.charger_geo = charger_geo
        self.charger_radius = charger_radius
        self.max_house_amps = max_house_amps
        self.max_car_amps = max_car_amps

        self.state = {}
        self.state_time = datetime(1970, 1, 1)
        self.is_enabled = is_enabled

        self.car_in_location = False
        self.car_distance_to_location = math.inf

    @asynccontextmanager
    async def with_latest_data(self) -> AsyncGenerator[TessieCarState]:
        if datetime.now() > self.state_time + self.api_cache_time:
            self.state = await self.force_refresh_state()
            self.state_time = datetime.now()
        yield TessieCarState(self.state)

    @backoff.on_exception(backoff.expo, asyncio.TimeoutError, max_time=60)
    async def force_refresh_state(self) -> TessieCarState:
        logger.debug(">>>> API CALL >>>> Refreshing state.")
        async with self.aiohttp_session.get(
            f"{tessie_api}{vehicle_vin}/state",
            headers={
                "accept": "application/json",
                "authorization": "Bearer " + tessie_token,
            },
        ) as response:
            if response.status != 200:
                print("Got headers: ", response.headers)
                raise Exception(await response.text())
            return TessieCarState(await response.json())
        
    async def force_refresh_state_delayed(self, delay_secs: int = 10):
        await asyncio.sleep(delay_secs)
        await self.force_refresh_state()

    @property
    def current_car_amps(self) -> float:
        if self.state["charge_state"]["charging_state"] != "Charging":
            return 0
        return self.state["charge_state"]["charge_amps"]

    async def tick_sliding_window(self):
        # print(" ", end=".", flush=True)
        if not self.is_enabled:
            return
        async with self.with_latest_data():
            self.current_house_amps = await self.request_current_house_amps()
            logger.debug("Current house & car amps: %f, %f", self.current_house_amps, self.current_car_amps)
            self.wattmeter_sliding_window.append(self.current_house_amps)
            self.car_amps_sliding_window.append(self.current_car_amps)
            while (
                len(self.wattmeter_sliding_window)
                > self.wattmeter_sliding_window_size
                / self.wattmeter_sliding_window_resolution
            ):
                _house_oldest = self.wattmeter_sliding_window.pop(0)
                _car_oldest = self.car_amps_sliding_window.pop(0)
                # logger.debug(
                #     "Removed from both windows: %f, %f",
                #     _house_oldest,
                #     _car_oldest,
                # )
                pass    

    async def request_current_house_amps(self) -> float:
        raise NotImplementedError("request_current_house_amps")

    def weighted_avg(self, window):
        """
        Calculate the weighted average of a sliding window.

        The last 5 items of the window have 80% of the weight.
        The rest of the window has 20% of the weight.
        """
        if len(window) == 0:
            return 0
        elif len(window) < 6:
            return sum(window) / len(window)

        less_important_values = sum(window[:-5]) / len(window[:-5])
        more_important_values = sum(window[-5:]) / len(window[-5:])

        return less_important_values * 0.2 + more_important_values * 0.8

    async def tick_set_car_amps(self):
        """
        Use the sliding window to determine the current car amps.

        - Never take more than 80% of the avg. house amps.
        - Don't take more than the max car amps.
        - The first 20% of the sliding window has 80% of the weight.
        - The last 80% of the sliding window has 20% of the weight.
        - Calculate the max car amps as the max house amps (the total budget) minus the actual weighted consumption.
        - Don't overreact. If undershot by 1A, keep the same amps.
        - Only call the API if the car amps need to be changed.
        """
        if len(self.wattmeter_sliding_window) == 0:
            logger.warn("tick_set_car_amps: Empty sliding window. Skipping.")
            return
        if self.is_enabled is False:
            logger.info("tick_set_car_amps: Disabled. Skipping.")
            return
        if self.state["charge_state"]["charging_state"] != "Charging":
            logger.info("Car is not charging. Skipping.")
            return

        # Calculate the weighted average
        weighted_avg_house_amps = self.weighted_avg(self.wattmeter_sliding_window)

        logger.info("Current house amps: %f", weighted_avg_house_amps)

        # Calculate the weighted average car amps
        weighted_avg_car_amps = self.weighted_avg(self.car_amps_sliding_window)

        logger.info("Current car amps: %f", weighted_avg_car_amps)

        # The weighted_avg_house_amps includes the car amps, so we need to subtract them
        weighted_avg_house_amps -= weighted_avg_car_amps

        logger.info("Current house amps (without car): %f", weighted_avg_house_amps)
        logger.info("Target house amps: %f", self.max_house_amps)

        # Calculate the new car amps based on the weighted average house amps and the max house budgeted amps (use 80% of the budget)
        new_car_amps = min(
            self.max_car_amps,
            max(
                0,
                self.max_house_amps - weighted_avg_house_amps,
            ),
        )

        logger.debug(
            "New car amps: %f %f %f",
            new_car_amps,
            self.max_car_amps,
            self.max_house_amps,
        )
        new_car_amps = int(new_car_amps)

        # Don't overreact (but do react if we're off by more than 1A, or if we are overshooting)
        if new_car_amps == int(self.current_car_amps) or new_car_amps - 1 == int(
            self.current_car_amps
        ):
            logger.info(
                f"Not overreacting. Keeping the same car amps (target would be {new_car_amps}, was {self.current_car_amps})."
            )
            return

        # Set the new car amps
        logger.info(
            "Setting car charge amps to %f (was %f)",
            new_car_amps,
            self.current_car_amps,
        )
        await self.set_car_charge_amps(new_car_amps)
        asyncio.create_task(self.force_refresh_state_delayed(10))

    async def set_car_charge_amps(self, requested_amps: int):
        raise NotImplementedError("set_car_charge_amps")

    async def get_car_geo(self) -> GeoLocation:
        raise NotImplementedError("get_car_geo")

    async def get_car_distance_to_location(self) -> float:
        car_geo = await self.get_car_geo()
        return get_distance(car_geo, self.charger_geo)

    async def is_car_nearby(self) -> bool:
        distance = await self.get_car_distance_to_location()
        return distance < self.charger_radius

    async def car_min_time_to_charger(self, max_speed_kmh=150) -> timedelta:
        distance = await self.get_car_distance_to_location()
        car_max_speed_ms = max_speed_kmh / 3.6
        return timedelta(seconds=int(distance // car_max_speed_ms))


class MockChargingState(ChargingState):
    mock_file = "mock.json"

    async def force_refresh_state(self) -> TessieCarState:
        return {
            "access_type": "mock",
            "api_version": 1,
            "display_name": "Mock",
            "charge_state": {
                "charge_amps": 0,
                "charge_current_request": 0,
            },
            "drive_state": {
                "gps_as_of": 0,
                "latitude": 0,
                "longitude": 0,
                "heading": 0,
                "speed": 0,
            },
        }

    def get_mock_info(self):
        with open(self.mock_file, "r") as f:
            return json.load(f)

    async def request_current_house_amps(self) -> float:
        return self.get_mock_info()["house_amps"]

    async def set_car_charge_amps(self, requested_amps: int):
        logger.info(">>>> API CALL >>>> Setting car charge amps to %f", requested_amps)

    async def get_car_geo(self) -> GeoLocation:
        return GeoLocation(**self.get_mock_info()["car_geo"])

    @property
    def current_car_amps(self) -> float:
        return self.get_mock_info()["car_amps"]


class LiveChargingState(ChargingState):
    last_requested_amps = 0

    @backoff.on_exception(backoff.expo, asyncio.TimeoutError, max_time=60)
    async def request_current_house_amps(self) -> float:
        async with self.aiohttp_session.get(wattmeter_api) as response:
            return float(await response.text())

    @backoff.on_exception(backoff.expo, asyncio.TimeoutError, max_time=60)
    async def set_car_charge_amps(self, requested_amps: int):
        if requested_amps == self.last_requested_amps:
            logger.info("Not sending request to API. Already asked but RealWorld was slow to react.")
            return
        
        logger.info(">>>> API CALL >>>> Setting car charge amps to %f", requested_amps)

        self.last_requested_amps = requested_amps
        return await self.aiohttp_session.post(
            f"{tessie_api}{vehicle_vin}/command/set_charging_amps?amps={requested_amps}",
            headers={
                "accept": "application/json",
                "authorization": "Bearer " + tessie_token,
            },
        )

    async def get_car_geo(self) -> GeoLocation:
        async with self.with_latest_data() as state:
            return GeoLocation(
                lat=state["drive_state"]["latitude"],
                lon=state["drive_state"]["longitude"],
            )


def notify_user_of_high_current(amps: float):
    if os.name == "posix":
        if os.uname().sysname == "Darwin":
            os.system(
                f'osascript -e \'display notification "{amps}" with title "High current detected"\''
            )
        else:
            os.system(f'notify-send "High current detected" "{amps}"')
    elif os.name == "nt":
        os.system(f"echo High current detected: {amps} | msg *")


def looping_task(func, every_seconds) -> asyncio.Task:
    async def loop():
        while True:
            await func()
            await asyncio.sleep(every_seconds)

    return asyncio.create_task(loop())


def is_car_nearby(cache: ChargingState):
    async def loop():
        cache.is_enabled = False
        while True:
            logger.debug("[CHECK] Car nearby? ")
            if not await cache.is_car_nearby():
                logger.debug("[CHECK] nearby = False")
                car_max_speed_kmh = 150
                car_min_time_to_charger = await cache.car_min_time_to_charger()
                distance = await cache.get_car_distance_to_location()
                distance = round(distance, 2)
                logger.debug(
                    f"It will take the car at least {car_min_time_to_charger} to get to the charger (at {car_max_speed_kmh} km/h, {distance}m away)"
                )
                logger.info(f"Sleeping for {car_min_time_to_charger}.")
                await asyncio.sleep(car_min_time_to_charger.total_seconds())
                continue
            else:
                distance = await cache.get_car_distance_to_location()
                logger.info(f"[CHECK] nearby = True  # ({distance} meters)")
                cache.is_enabled = True
                break
    
    return asyncio.create_task(loop())



@click.command()
@click.option("--every_seconds", type=int, default=1)
# @click.option("--threshold", type=float, default=11.3)
@click.option("--threshold", type=float, default=10.8)
@click.option("--warn_after_threshold", type=int, default=2)
@click.option("--max_car_amps", type=int, default=10)
@click.option("--persistent-cache", is_flag=True, default=True)
@click.option("--check-location", is_flag=True)
@click.option("--test", is_flag=True)
async def cli(
    threshold,
    every_seconds,
    warn_after_threshold,
    max_car_amps,
    persistent_cache,
    check_location,
    test,
):
    # Do this in asyncio
    # Create a loop

    main_class = MockChargingState if test else LiveChargingState

    exit = False

    while not exit:

        try:
            async with aiohttp.ClientSession(
                timeout=aiohttp.ClientTimeout(total=10)
            ) as session:
                if persistent_cache and os.path.exists("cache.pickle"):
                    with open("cache.pickle", "rb") as f:
                        cache = pickle.load(f)
                        if not isinstance(cache, main_class):
                            logger.warn("Invalid cache. Initializing new cache.")
                            cache = main_class(aiohttp_session=session)
                else:
                    cache = main_class(aiohttp_session=session)
                
                cache.aiohttp_session = session
                cache.charger_geo = charger_geo
                cache.charger_radius = charger_radius
                cache.max_car_amps = max_car_amps
                cache.max_house_amps = threshold
                cache.is_enabled = True

                while True:
                    if check_location:
                        task_location = is_car_nearby(cache)
                        await asyncio.wait([task_location])

                    task1 = looping_task(cache.tick_sliding_window, every_seconds)
                    task2 = looping_task(cache.tick_set_car_amps, every_seconds * 5)
                    r = await asyncio.wait([task1, task2], return_when=asyncio.FIRST_EXCEPTION)
                    raise r[0].pop().exception()
                
        except asyncio.CancelledError:
            exit = True
            logger.info("KeyboardInterrupt. Exiting.")
            task1.cancel()
            task2.cancel()

        except BaseException as e:
            logger.error(f"Exception: {type(e)}")
            logger.exception(e)
            task1.cancel()
            task2.cancel()

        finally:
            if persistent_cache:
                with open("cache.pickle", "wb") as f:
                    cache.aiohttp_session = None
                    cache.is_enabled = True
                    pickle.dump(cache, f)
            logger.info("Exiting.")
            if not exit:
                await asyncio.sleep(15)


def test_check_location():
    assert (
        get_distance(GeoLocation(lat=0.0, lon=0.0), GeoLocation(lat=0.0, lon=0.0)) == 0
    )

    assert math.isclose(
        get_distance(
            # Arlington to London
            GeoLocation(lat=51.5, lon=0.0),
            GeoLocation(lat=38.8, lon=-77.1),
        ),
        5918000,
        abs_tol=200,
    )


def test_weighted_avg():
    scenario = [1] * 50 + [5] * 5
    expectation = 5 * 0.8 + 1 * 0.2
    assert math.isclose(
        MockChargingState(None).weighted_avg(scenario), expectation
    ), f"{MockChargingState(None).weighted_avg(scenario)} != {expectation}"


def tests():
    test_check_location()
    test_weighted_avg()


if __name__ == "__main__":
    tests()
    cli()
