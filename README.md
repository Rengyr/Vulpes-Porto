Vulpes Porto\
Dominik 'Rengyr' Kosík, <mail@rengyr.eu>

Mastodon bot to post remotely hosted photos daily at set times. The bot will prioritize images that weren't yet posted. In case of exhaustion of new photos the bot will post random photo from the pool.

The bot takes one mandatory argument which is the location of the configuration json file and this needs to be first argument. The bot supports  two optional options:

`--now` that will cause the bot to post one image on start-up and then continue based on the schedule in the configuration json file

`--systemd` that will prefix the output messages with log levels according to Linux syslog specification. The levels used are err, notice and info

Example of starting bot:
```
Normal start:
./vulpes_porto ./config.json

Start with posting one image immediately:
./vulpes_porto ./config.json --now
```

Server side configuration example is in config.json. This file contains configuration for mastodon server, token of the bot, address of json with links of remote photos that will be described later, times when to post photos each day and location for file for tracking used and still unused photos.

Remote json file has following structure: list of records where each record has "location" which is a link to the remotely hosted photo. Each record can have optional message that will get posted with the photo and optional alternative text for the photo.

Example of json structure:
```
[
    {
	"msg": "Augsburg Zoo, Germany",
	"location": "https://example.com/sources/IMG_1978.JPG"
    },
    {
	"location": "https://example.com/fennecbot/sources/IMG_1955.JPG"
    },
    {
	"msg": "Augsburg Zoo, Germany",
	"location": "https://example.com/fennecbot/sources/0001.jpg",
	"alt": "Fennec sitting on a sand dune"
    },
]
```
