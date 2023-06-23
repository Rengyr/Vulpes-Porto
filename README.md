Vulpes Porto\
Dominik 'Rengyr' Kosík, <mail@rengyr.eu>

[![Project status](https://github.com/Rengyr/Vulpes-Porto/actions/workflows/rust.yml/badge.svg)](https://github.com/Rengyr/Vulpes-Porto/actions/workflows/rust.yml)

Mastodon bot to post remotely hosted photos daily at set times. The bot will prioritize images that weren't yet posted. In case of exhaustion of new photos the bot will post random photo from the pool.

The bot takes one mandatory argument which is the location of the configuration json file and this needs to be first argument. The bot supports two optional options:

`--now` that will cause the bot to post one image on start-up and then continue based on the schedule in the configuration json file

Example of starting bot:
```
Normal start:
./vulpes_porto ./config.json

Start with posting one image immediately:
./vulpes_porto ./config.json --now
```

Server side configuration example is in config.json. This file contains configuration for mastodon server, token of the bot, location of json with links of local or remote photos that will be described later, times when to post photos each day and location for file for tracking used and still unused photos.

In the server side configuration file syslog style errors and error level can be set.

Json file with links to images has following structure: list of records where each record has "location" which is a link to the remotely hosted photo or local photo. Each record can have optional message that will get posted with the photo, optional alternative text for the photo and optional content warning.

Using local photos requires prefix "file:" in the "location" field in the json. Using local photos as well requires to have setup "local_path" in server side configuration file (see config.json example).

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
	{
	"msg": "Somewhere on field, Germany",
	"location": "https://example.com/fennec/sources/0002.jpg",
	"alt": "Fennec sitting on a grass",
	"content_warning": "Dangerously beautiful fox"
    },
	{
	"msg": "Augsburg Zoo, Germany",
	"location": "file:fox.jpg"
    }
]
```

Example of mastodon account that is using Vulpes Porto:\
[@toomanyfoxes@botsin.space](https://botsin.space/@toomanyfoxes)
