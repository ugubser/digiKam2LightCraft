"""Generate a tiny synthetic SQLite catalog and procedural PNGs; test tooling only."""
import sqlite3, sys, struct, zlib
from pathlib import Path

base=Path(sys.argv[1]);base.mkdir(parents=True,exist_ok=True)
photos=base/'photos';photos.mkdir();(photos/'日本語 fête').mkdir()
# Original procedural 2x2 PNG, not downloaded media.
def chunk(kind,data): return struct.pack('>I',len(data))+kind+data+struct.pack('>I',zlib.crc32(kind+data)&0xffffffff)
png=b'\x89PNG\r\n\x1a\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',2,2,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(b'\0\xff\0\0\0\xff\0\0\0\0\xff\xff\xff\xff'))+chunk(b'IEND',b'')
for name in ['one.ORF','two.PNG','movie.MOV','gps.PNG','heif.HEIC','日本語 fête/unicode.PNG']: (photos/name).write_bytes(png)
(base/'database').mkdir()
c=sqlite3.connect(base/'database'/'digikam4.db')
c.executescript('''
CREATE TABLE Settings(keyword TEXT,value TEXT);
INSERT INTO Settings VALUES('databaseUUID','synthetic-uuid'),('DBVersion','17');
CREATE TABLE AlbumRoots(id INTEGER PRIMARY KEY,label TEXT,specificPath TEXT);
INSERT INTO AlbumRoots VALUES(1,'Photos','/old-machine/Photos');
CREATE TABLE Albums(id INTEGER PRIMARY KEY,albumRoot INTEGER,relativePath TEXT,caption TEXT);
INSERT INTO Albums VALUES(1,1,'/',''),(2,1,'/日本語 fête','');
CREATE TABLE Images(id INTEGER PRIMARY KEY,album INTEGER,name TEXT,status INTEGER,category INTEGER,fileSize INTEGER,uniqueHash TEXT);
INSERT INTO Images VALUES(1,1,'one.ORF',1,1,100,'not-a-lightcraft-hash'),(2,1,'two.PNG',1,1,100,'h'),(3,1,'movie.MOV',1,2,100,'h'),(4,1,'missing.JPG',1,1,100,'h'),(5,NULL,'deleted.PNG',4,1,100,'h'),(6,2,'unicode.PNG',1,1,100,'h'),(7,1,'gps.PNG',1,1,100,'h'),(8,1,'heif.HEIC',1,1,100,'h');
CREATE TABLE ImageInformation(imageid INTEGER PRIMARY KEY,rating INTEGER,creationDate TEXT,width INTEGER,height INTEGER,orientation INTEGER,format TEXT);
INSERT INTO ImageInformation VALUES(1,2,'2001-02-03T04:05:06',400,300,8,'RAW-ORF'),(2,-1,'2002-02-03T04:05:06',2,2,1,'PNG'),(3,0,NULL,1920,1080,1,'MOV'),(4,5,NULL,400,300,1,'JPG'),(5,5,NULL,2,2,1,'PNG'),(6,1,'1904-01-01T00:00:00',2,2,1,'PNG'),(7,0,NULL,2,2,1,'PNG'),(8,3,NULL,2,2,1,'HEIF');
CREATE TABLE ImageMetadata(imageid INTEGER PRIMARY KEY,make TEXT,model TEXT,lens TEXT,aperture REAL,focalLength REAL,exposureTime REAL,sensitivity INTEGER);
INSERT INTO ImageMetadata VALUES(1,'OLYMPUS','TEST','TEST LENS',2.8,25,0.004,200);
CREATE TABLE ImagePositions(imageid INTEGER PRIMARY KEY,latitudeNumber REAL,longitudeNumber REAL,altitude REAL);
INSERT INTO ImagePositions VALUES(1,47.0,8.0,400),(7,91,8,0);
CREATE TABLE ImageComments(id INTEGER PRIMARY KEY,imageid INTEGER,type INTEGER,language TEXT,comment TEXT);
INSERT INTO ImageComments VALUES(1,1,1,'x-default','database caption'),(2,1,3,'x-default','database title');
CREATE TABLE Tags(id INTEGER PRIMARY KEY,pid INTEGER,name TEXT);
INSERT INTO Tags VALUES(1,0,'_Digikam_Internal_Tags_'),(2,1,'Color Label Magenta'),(3,1,'Pick Label Pending'),(4,0,'Travel'),(5,4,'日本'),(6,0,'People'),(7,6,'Test Person'),(8,6,'Unknown');
CREATE TABLE TagProperties(tagid INTEGER,property TEXT,value TEXT);
INSERT INTO TagProperties VALUES(1,'internalTag',NULL),(2,'internalTag',NULL),(3,'internalTag',NULL),(7,'person',NULL),(8,'unknownPerson',NULL),(8,'person',NULL);
CREATE TABLE ImageTags(imageid INTEGER,tagid INTEGER);
INSERT INTO ImageTags VALUES(1,2),(1,3),(1,5),(1,7);
CREATE TABLE ImageTagProperties(imageid INTEGER,tagid INTEGER,property TEXT,value TEXT);
INSERT INTO ImageTagProperties VALUES(1,7,'tagRegion','<rect x="30" y="50" width="60" height="80"/>'),(2,8,'autodetectedFace','<rect x="0" y="0" width="1" height="1"/>');
CREATE TABLE ImageRelations(subject INTEGER,object INTEGER,type INTEGER);
INSERT INTO ImageRelations VALUES(2,1,2);
CREATE TABLE VideoMetadata(imageid INTEGER PRIMARY KEY,duration TEXT);
INSERT INTO VideoMetadata VALUES(3,'12345');
CREATE TABLE FutureMetadata(imageid INTEGER,payload BLOB);
INSERT INTO FutureMetadata VALUES(1,X'00FFCAFE');
''')
c.commit();c.close()
full='''<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:tiff="http://ns.adobe.com/tiff/1.0/" xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmp:Rating="4" tiff:Orientation="8" crs:Exposure2012="1.0"><dc:description><rdf:Alt><rdf:li xml:lang="x-default">sidecar caption</rdf:li></rdf:Alt></dc:description><mwg-rs:Regions xmlns:mwg-rs="http://www.metadataworkinggroup.com/schemas/regions/" rdf:parseType="Resource"><mwg-rs:RegionList><rdf:Bag><rdf:li rdf:parseType="Resource"><mwg-rs:Name>Test Person</mwg-rs:Name><mwg-rs:Type>Face</mwg-rs:Type><mwg-rs:Area xmlns:stArea="http://ns.adobe.com/xmp/sType/Area#" stArea:x="0.775" stArea:y="0.2" stArea:w="0.2" stArea:h="0.2" stArea:unit="normalized"/></rdf:li></rdf:Bag></mwg-rs:RegionList></mwg-rs:Regions></rdf:Description></rdf:RDF></x:xmpmeta>'''
(photos/'one.ORF.xmp').write_text(full)
(photos/'one.xmp').write_text(full.replace('Exposure2012="1.0"','Exposure2012="2.0"').replace('Rating="4"','Rating="1"'))
