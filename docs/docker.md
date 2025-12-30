# Creating a dora docker image

After checking out the source, build dora in docker and create an image:

```
docker build -t dora .
```

Next, create a `data` directory if it does not exist, and put `config.yaml` in it. This directory will be used to read the config and to store your leases database file.

```
mkdir data
touch data/config.yaml
```

(edit config.yaml)

Then run the image you created with `--net=host` and with the data dir volume mounted:

```
docker run -it --rm --init --net=host -v "$(pwd)/data":/var/lib/dora dora
```
